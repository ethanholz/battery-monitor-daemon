use anyhow::Result;
use battery::{units::ratio::percent, State};
use clap::Parser;
use core::fmt;
use gethostname::gethostname;
use rumqttc::{AsyncClient, MqttOptions, QoS};
use serde::Serialize;
use std::{mem, time::Duration};
use tokio::{sync::mpsc, task, time};

#[derive(Parser)]
#[command(author, version, about, long_about=None)]
struct Args {
    #[arg(short, long, default_value = "battery-daemon/status/battery")]
    topic: String,

    #[arg(long, default_value = "localhost")]
    hostname: String,

    #[arg(short, long, default_value_t = 1883)]
    port: u16,

    #[arg(long, default_value = "homeassistant")]
    discovery_topic: String,
}

#[derive(PartialEq, Serialize, Clone, Copy)]
struct ChargeInfo {
    percentage: f32,
    #[serde(with = "StateDef")]
    state: State,
}

#[derive(Serialize)]
#[serde(remote = "State")]
enum StateDef {
    Unknown,
    Charging,
    Discharging,
    Empty,
    Full,
    __Nonexhaustive,
}

#[derive(PartialEq, Serialize)]
struct DiscoveryPayload {
    name: String,
    device_class: String,
    state_topic: String,
    unit_of_measurement: String,
    value_template: String,
}

impl DiscoveryPayload {
    fn new(
        name: String,
        device_class: String,
        state_topic: String,
        unit_of_measurement: String,
        value_template: String,
    ) -> DiscoveryPayload {
        DiscoveryPayload {
            name,
            device_class,
            state_topic,
            unit_of_measurement,
            value_template,
        }
    }
}

struct DiscoveryPayloadBuilder {
    name: String,
    device_class: String,
    state_topic: String,
}

impl DiscoveryPayloadBuilder {
    fn new() -> DiscoveryPayloadBuilder {
        DiscoveryPayloadBuilder {
            name: String::from(""),
            device_class: String::from(""),
            state_topic: String::from(""),
        }
    }

    fn name(mut self, name: String) -> DiscoveryPayloadBuilder {
        self.name = name;
        self
    }

    fn device_class(mut self, device_class: String) -> DiscoveryPayloadBuilder {
        self.device_class = device_class;
        self
    }

    fn state_topic(mut self, state_topic: String) -> DiscoveryPayloadBuilder {
        self.state_topic = state_topic;
        self
    }

    // fn build(self) -> DiscoveryPayload {
    //     DiscoveryPayload {
    //         name: self.name,
    //         device_class: self.device_class,
    //         state_topic: self.state_topic,
    //     }
    // }
}

impl fmt::Display for DiscoveryPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Ok(payload) = serde_json::to_string(self) {
            write!(f, "{}", payload)
        } else {
            panic!("Failed to serialize payload")
        }
    }
}

#[derive(PartialEq)]
struct DiscoveryTopic {
    discovery_prefix: String,
    comp: DiscoveryDevice,
    node_id: NodeID,
    object_id: String,
}

impl fmt::Display for DiscoveryTopic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.node_id {
            NodeID::Empty => write!(
                f,
                "{}/{}/{}/config",
                self.discovery_prefix, self.comp, self.object_id
            ),
            NodeID::Is(id) => write!(
                f,
                "{}/{}/{}/{}/config",
                self.discovery_prefix, id, self.comp, self.object_id
            ),
        }
    }
}

struct DiscoveryTopicBuilder {
    discovery_prefix: String,
    comp: DiscoveryDevice,
    node_id: NodeID,
    object_id: String,
}

impl DiscoveryTopicBuilder {
    fn new() -> DiscoveryTopicBuilder {
        if let Ok(hostname) = gethostname().into_string() {
            DiscoveryTopicBuilder {
                discovery_prefix: String::from("homeassistant"),
                comp: DiscoveryDevice::NoneType,
                node_id: NodeID::Empty,
                object_id: hostname,
            }
        } else {
            DiscoveryTopicBuilder {
                discovery_prefix: String::from("homeassistant"),
                comp: DiscoveryDevice::NoneType,
                node_id: NodeID::Empty,
                object_id: String::from(""),
            }
        }
    }
    fn build(self) -> DiscoveryTopic {
        DiscoveryTopic {
            discovery_prefix: self.discovery_prefix,
            comp: self.comp,
            node_id: self.node_id,
            object_id: self.object_id,
        }
    }
    fn comp(mut self, comp: DiscoveryDevice) -> DiscoveryTopicBuilder {
        self.comp = comp;
        self
    }
}

struct Discovery {
    topic: DiscoveryTopic,
    payload: DiscoveryPayload,
}

#[derive(PartialEq)]
enum DiscoveryDevice {
    BinarySensor,
    Sensor,
    NoneType,
}

impl fmt::Display for DiscoveryDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::BinarySensor => return write!(f, "binary_sensor"),
            Self::Sensor => return write!(f, "sensor"),
            _ => return write!(f, "none"),
        };
    }
}

#[derive(PartialEq)]
enum NodeID {
    Empty,
    Is(String),
}

#[derive(PartialEq)]
struct Message {
    topic: String,
    payload: String,
    retain: bool,
}

struct MessageBuilder {
    topic: String,
    payload: String,
    retain: bool,
}

impl MessageBuilder {
    fn new() -> MessageBuilder {
        MessageBuilder {
            topic: String::from(""),
            payload: String::from(""),
            retain: false,
        }
    }

    fn build(self) -> Message {
        Message {
            topic: self.topic,
            payload: self.payload,
            retain: self.retain,
        }
    }
    fn retain(mut self, retain: bool) -> MessageBuilder {
        self.retain = retain;
        self
    }

    fn topic(mut self, topic: String) -> MessageBuilder {
        self.topic = topic;
        self
    }

    fn payload(mut self, payload: String) -> MessageBuilder {
        self.payload = payload;
        self
    }
}

impl From<Discovery> for MessageBuilder {
    fn from(value: Discovery) -> MessageBuilder {
        MessageBuilder {
            topic: value.topic.to_string(),
            payload: value.payload.to_string(),
            retain: false,
        }
    }
}

async fn home_assistant_discovery(
    client: AsyncClient,
    topic: DiscoveryTopic,
    payload: DiscoveryPayload,
) {
    let discovery = Discovery { topic, payload };
    let message: Message = MessageBuilder::from(discovery).retain(true).build();
    mqtt_send(client, message).await;
}

async fn mqtt_send(client: AsyncClient, message: Message) {
    match client
        .publish(
            message.topic,
            QoS::AtLeastOnce,
            message.retain,
            message.payload.clone(),
        )
        .await
    {
        Err(e) => println!("Client error: {:?}", e),
        _ => println!("sending {}", &message.payload),
    }
}

fn get_charge_info() -> Result<ChargeInfo> {
    let manager = battery::Manager::new()?;
    let mut percentage = 0.0;
    let mut state = State::Unknown;
    for (_, dev) in manager.batteries()?.enumerate() {
        let battery = dev?;
        percentage = battery.state_of_charge().get::<percent>();
        state = battery.state();
    }
    let info = ChargeInfo { percentage, state };
    Ok(info)
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let port = args.port;
    let hostname = args.hostname;
    let topic = args.topic;
    let state_topic = format!("{}/state", topic);

    let (tx, mut rx) = mpsc::channel(mem::size_of::<Message>());

    let mut options = MqttOptions::new(&topic, &hostname, port);
    options.set_keep_alive(Duration::from_secs(10));
    let (client, mut eventloop) = AsyncClient::new(options, 10);

    let discovery_topic: DiscoveryTopic = DiscoveryTopicBuilder::new()
        .comp(DiscoveryDevice::Sensor)
        .build();
    let discovery_payload = DiscoveryPayload::new(
        discovery_topic.object_id.clone(),
        DiscoveryDevice::Sensor.to_string(),
        state_topic.clone(),
        String::from("%"),
        String::from("{{ value_json.percentage }}"),
    );
    home_assistant_discovery(client.clone(), discovery_topic, discovery_payload).await;

    task::spawn(async move {
        let mut prev_info = ChargeInfo {
            percentage: 0.0,
            state: State::Unknown,
        };
        loop {
            let info = get_charge_info();
            let value = match info {
                Ok(x) => x,
                Err(_) => ChargeInfo {
                    percentage: 0.0,
                    state: State::Unknown,
                },
            };
            if value != prev_info {
                let payload = match serde_json::to_string(&value) {
                    Ok(j) => j,
                    _ => String::from("parsing error"),
                };
                let message = MessageBuilder::new()
                    .payload(payload.clone())
                    .topic(state_topic.clone())
                    .retain(true)
                    .build();
                if let Err(_) = tx.send(message).await {
                    println!("receiver dropped")
                }
                prev_info = value;
            }
            time::sleep(Duration::from_secs(60)).await;
        }
    });

    task::spawn(async move {
        loop {
            if let Some(info) = rx.recv().await {
                mqtt_send(client.clone(), info).await;
            };
            time::sleep(Duration::from_secs(60)).await;
        }
    });
    loop {
        match eventloop.poll().await {
            Ok(_) => (),
            Err(e) => println!("{:?}", e),
        }
    }
}
