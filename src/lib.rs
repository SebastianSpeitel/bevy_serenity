#![allow(clippy::needless_pass_by_value)]
use std::ops::Deref;
use std::pin::Pin;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use async_compat::Compat;
use bevy_ecs::prelude::*;
use bevy_utils::{
    futures::check_ready,
    tracing::{error, info, trace},
};
pub use serenity;
use serenity::{client::ClientBuilder, prelude::*};

type StartingFut =
    Compat<Pin<Box<dyn std::future::Future<Output = serenity::Result<serenity::Client>>>>>;

#[derive(Component)]
struct Starting(OnceLock<StartingFut>);

unsafe impl Send for Starting {}
unsafe impl Sync for Starting {}

type RunningFut = Compat<Pin<Box<dyn std::future::Future<Output = ()> + Send + Sync>>>;

#[derive(Component)]
struct Running(OnceLock<RunningFut>);

#[derive(Component, Clone, Debug)]
pub struct SerenityConfig {
    pub token: String,
    pub intents: GatewayIntents,
}

#[derive(Component)]
pub struct SerenityClient(Arc<serenity::http::Http>);

impl Deref for SerenityClient {
    type Target = serenity::http::Http;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

type RawEvent = (serenity::all::Event, Context);

#[derive(Event, Debug)]
pub struct SerenityEvent {
    pub entity: Entity,
    pub event: serenity::all::Event,
    pub context: Context,
}

struct SenderHandler(Sender<RawEvent>);

#[serenity::async_trait]
impl RawEventHandler for SenderHandler {
    async fn raw_event(&self, ctx: Context, ev: serenity::all::Event) {
        trace!(message = "Received raw event", event = ?ev);
        self.0.send((ev, ctx)).unwrap();
    }
}

#[derive(Component)]
struct RawEventReceiver(Arc<Mutex<Receiver<RawEvent>>>);

impl From<Receiver<RawEvent>> for RawEventReceiver {
    fn from(receiver: Receiver<RawEvent>) -> Self {
        Self(Arc::new(Mutex::new(receiver)))
    }
}

async fn start_client(
    config: SerenityConfig,
    sender: Sender<RawEvent>,
) -> serenity::Result<serenity::Client> {
    let client = ClientBuilder::new(config.token, config.intents)
        .raw_event_handler(SenderHandler(sender))
        .await?;
    Ok(client)
}

fn start(mut commands: Commands, clients: Query<(Entity, &SerenityConfig), Added<SerenityConfig>>) {
    for (client, config) in &clients {
        info!(message = "Starting client", ?config);
        let (sender, receiver) = channel();
        let fut = start_client(config.to_owned(), sender);
        let boxed_fut: Pin<Box<dyn std::future::Future<Output = _>>> = Box::pin(fut);
        let fut = Compat::new(boxed_fut);
        let cell = OnceLock::new();
        let _ = cell.set(fut);
        commands
            .entity(client)
            .insert((Starting(cell), RawEventReceiver::from(receiver)));
    }
}

fn poll_starting(mut commands: Commands, mut clients: Query<(Entity, &mut Starting)>) {
    for (entity, mut starting) in &mut clients {
        let fut = starting.0.get_mut().unwrap();

        let Some(res) = check_ready(fut) else {
            continue;
        };
        commands.entity(entity).remove::<Starting>();
        let mut c = match res {
            Ok(c) => c,
            Err(e) => {
                error!(message = "Failed to start client", error = %e);
                continue;
            }
        };
        info!(message = "Client started");
        let client = SerenityClient(c.http.clone());
        let fut = async move {
            let _ = c.start().await;
        };
        let fut: Pin<Box<dyn std::future::Future<Output = ()> + Send + Sync>> = Box::pin(fut);
        let fut = Compat::new(fut);
        let cell = OnceLock::new();
        let _ = cell.set(fut);
        commands.entity(entity).insert((Running(cell), client));
    }
}

fn write_events(
    receivers: Query<(Entity, &RawEventReceiver)>,
    mut writer: EventWriter<SerenityEvent>,
) {
    for (entity, receiver) in &receivers {
        let receiver = receiver.0.lock().unwrap();
        for (event, context) in receiver.try_iter() {
            let ev = SerenityEvent {
                entity,
                event,
                context,
            };
            writer.send(ev);
        }
    }
}

fn run(mut clients: Query<&mut Running>) {
    for mut running in &mut clients {
        let fut = running.0.get_mut().unwrap();
        check_ready(fut);
    }
}

pub struct SerenityPlugin;

impl bevy_app::Plugin for SerenityPlugin {
    fn build(&self, app: &mut bevy_app::App) {
        app.add_event::<SerenityEvent>();
        app.add_systems(bevy_app::Update, (start, poll_starting, write_events, run));
    }
}
