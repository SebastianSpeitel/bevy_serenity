use std::pin::Pin;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use async_compat::Compat;
use bevy_ecs::prelude::*;
use futures_lite::future;
pub use serenity;
use serenity::{client::ClientBuilder, prelude::*};

type StartingFut =
    Compat<Pin<Box<dyn std::future::Future<Output = serenity::Result<serenity::Client>>>>>;

#[derive(Component)]
struct Starting(OnceLock<StartingFut>);

unsafe impl Send for Starting {}
unsafe impl Sync for Starting {}

#[derive(Component, Clone)]
pub struct SerenityConfig {
    pub token: String,
    pub intents: GatewayIntents,
}

#[derive(Component)]
pub struct SerenityClient(serenity::Client);

#[derive(Resource, Component)]
pub struct Http(pub Arc<serenity::http::Http>);

type RawEvent = (serenity::all::Event, Context);

#[derive(Event)]
pub struct SerenityEvent {
    pub entity: Entity,
    pub event: serenity::all::Event,
    pub context: Context,
}

struct SenderHandler(Sender<RawEvent>);

#[serenity::async_trait]
impl RawEventHandler for SenderHandler {
    async fn raw_event(&self, ctx: Context, ev: serenity::all::Event) {
        log::trace!("Received raw event: {:?}", ev);
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
    let client = ClientBuilder::new(&config.token, config.intents)
        .raw_event_handler(SenderHandler(sender))
        .await?;
    Ok(client)
}

fn start(mut commands: Commands, clients: Query<(Entity, &SerenityConfig), Added<SerenityConfig>>) {
    for (client, config) in &clients {
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
    for (client, mut starting) in &mut clients {
        let fut = starting.0.get_mut().unwrap();

        let Some(res) = future::block_on(future::poll_once(fut)) else {
            continue;
        };
        commands.entity(client).remove::<Starting>();
        let c = match res {
            Ok(c) => c,
            Err(e) => {
                log::error!("Failed to start client: {:?}", e);
                continue;
            }
        };
        commands
            .entity(client)
            .insert((Http(c.http.clone()), SerenityClient(c)));
    }
}

fn write_events(
    receivers: Query<(Entity, &RawEventReceiver)>,
    mut writer: EventWriter<SerenityEvent>,
) {
    for (entity, receiver) in &mut receivers.iter() {
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

pub struct SerenityPlugin;

impl bevy_app::Plugin for SerenityPlugin {
    fn build(&self, app: &mut bevy_app::App) {
        app.add_event::<SerenityEvent>();
        app.add_systems(bevy_app::Update, (start, poll_starting, write_events));
    }
}
