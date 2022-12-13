use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Mutex;

use bevy_ecs::prelude::*;
use bevy_tasks::IoTaskPool;
use log::{debug, trace};
use serenity::{client::ClientBuilder, prelude::*};

pub type RawEvent = (serenity::model::event::Event, Context);

enum Status {
    Connected(Client),
    Disconnected(SerenityError),
    ConnectFailure(SerenityError),
    Stopped,
}

#[derive(Resource)]
struct EventReceiver(Mutex<Receiver<RawEvent>>);

#[derive(Resource)]
struct StatusReceiver(Mutex<Receiver<Status>>);

#[derive(Resource)]
struct StatusSender(SyncSender<Status>);

struct RawHandler(SyncSender<RawEvent>);

#[serenity::async_trait]
impl RawEventHandler for RawHandler {
    async fn raw_event(&self, ctx: Context, ev: serenity::model::event::Event) {
        trace!("Received raw event: {:?}", ev);
        self.0.send((ev, ctx)).unwrap();
    }
}

fn start_client(mut client: Client, status_sender: SyncSender<Status>) {
    debug!("Starting client");
    let pool = IoTaskPool::get();
    pool.spawn(async move {
        use async_compat::Compat;
        use Status::*;

        let res = Compat::new(client.start()).await;
        match res {
            Err(e) => {
                debug!("Client disconnected: {:?}", e);
                status_sender.send(Disconnected(e)).unwrap();
            }
            Ok(_) => {
                debug!("Client stopped");
                status_sender.send(Stopped).unwrap();
            }
        }
    })
    .detach();
}

fn event_handler(receiver: Res<EventReceiver>, mut writer: EventWriter<RawEvent>) {
    let receiver = receiver.0.lock().unwrap();
    for ev in receiver.try_iter() {
        writer.send(ev);
    }
}

fn status_handler(receiver: Res<StatusReceiver>, sender: Res<StatusSender>) {
    use Status::*;
    let receiver = receiver.0.lock().unwrap();
    for status in receiver.try_iter() {
        match status {
            Connected(c) => {
                start_client(c, sender.0.clone());
            }
            ConnectFailure(e) => panic!("Failed to connect: {:?}", e),
            Stopped => panic!("Shutdown"),
            Disconnected(e) => panic!("Disconnect: {:?}", e),
        }
    }
}

pub struct SerenityPlugin {
    pub token: String,
    pub intents: GatewayIntents,
}

impl SerenityPlugin {
    pub fn new(token: impl Into<String>, intents: GatewayIntents) -> Self {
        Self {
            token: token.into(),
            intents,
        }
    }

    fn init_resources(&self, world: &mut bevy_ecs::world::World) {
        let pool = IoTaskPool::init(Default::default);

        let (ev_sender, ev_receiver) = sync_channel(32);
        let (status_sender, status_receiver) = sync_channel(32);

        let client =
            ClientBuilder::new(&self.token, self.intents).raw_event_handler(RawHandler(ev_sender));

        let sender = status_sender.clone();
        pool.spawn(async move {
            use async_compat::Compat;

            match Compat::new(client).await {
                Ok(client) => {
                    debug!("Client connected");
                    sender.send(Status::Connected(client)).unwrap();
                }
                Err(err) => {
                    debug!("Client failed to connect: {:?}", err);
                    sender.send(Status::ConnectFailure(err)).unwrap();
                }
            }
        })
        .detach();

        world.insert_resource(StatusSender(status_sender));
        world.insert_resource(StatusReceiver(status_receiver.into()));
        world.insert_resource(EventReceiver(Mutex::new(ev_receiver)));
    }
}

impl bevy_app::Plugin for SerenityPlugin {
    fn build(&self, app: &mut bevy_app::App) {
        app.add_event::<RawEvent>();
        app.add_event::<Status>();

        self.init_resources(&mut app.world);

        app.add_system(event_handler);
        app.add_system(status_handler);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_app::{prelude::*, ScheduleRunnerPlugin};

    fn mock_app() -> App {
        let mut app = App::new();
        app.add_plugin(ScheduleRunnerPlugin::default());
        app
    }

    #[test]
    fn inits_resources() {
        let mut app = mock_app();
        app.add_plugin(SerenityPlugin::new("token", GatewayIntents::all()));
        app.update();

        assert!(app.world.is_resource_added::<StatusSender>());
        assert!(app.world.is_resource_added::<StatusReceiver>());
        assert!(app.world.is_resource_added::<EventReceiver>());
    }
}
