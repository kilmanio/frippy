use irc::client::prelude::*;

use std::time::Duration;
use std::sync::Mutex;

use time;
use chrono::NaiveDateTime;
use humantime::format_duration;

use plugin::*;

use failure::Fail;
use failure::ResultExt;
use error::ErrorKind as FrippyErrorKind;
use error::FrippyError;
use self::error::*;

pub mod database;
use self::database::Database;

macro_rules! try_lock {
    ( $m:expr ) => {
        match $m.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[derive(PluginName, Default)]
pub struct Tell<T: Database> {
    tells: Mutex<T>,
}

impl<T: Database> Tell<T> {
    pub fn new(db: T) -> Tell<T> {
        Tell {
            tells: Mutex::new(db),
        }
    }

    fn tell_command(
        &self,
        client: &IrcClient,
        command: PluginCommand,
    ) -> Result<String, TellError> {
        if command.tokens.len() < 2 {
            return Ok(self.invalid_command().to_owned());
        }

        let mut online = Vec::new();

        let receivers = command.tokens[0].split(',').filter(|&s| !s.is_empty());
        let sender = command.source;

        let mut no_receiver = true;
        for receiver in receivers {
            if receiver.eq_ignore_ascii_case(client.current_nickname())
                || receiver.eq_ignore_ascii_case(&sender)
            {
                online.push(receiver);
                continue;
            }

            let channels = client
                .list_channels()
                .expect("The irc crate should not be compiled with the \"nochanlists\" feature");

            if let Some(_) = channels
                .iter()
                .map(|channel| client.list_users(&channel))
                .map(|option| {
                    option.and_then(|users| {
                        users.into_iter()
                            .find(|user| user.get_nickname().eq_ignore_ascii_case(&receiver))
                    })
                })
                .find(|option| option.is_some())
            {
                online.push(receiver);
                continue;
            }

            let tm = time::now().to_timespec();
            let message = command.tokens[1..].join(" ");
            let tell = database::NewTellMessage {
                sender: &sender,
                receiver: &receiver.to_lowercase(),
                time: NaiveDateTime::from_timestamp(tm.sec, 0u32),
                message: &message,
            };

            debug!("Saving tell for {:?}", receiver);
            try_lock!(self.tells).insert_tell(&tell)?;
            no_receiver = false;
        }

        Ok(if no_receiver {
            format!("Invalid receiver.")
        } else {
            match online.len() {
                0 => format!("Got it!"),
                1 => format!("{} is currently online.", online[0]),
                _ => format!("{} are currently online.", online.join(", ")),
            }
        })
    }

    fn on_namelist(&self, client: &IrcClient, channel: &str) -> Result<(), FrippyError> {
        let receivers = try_lock!(self.tells)
            .get_receivers()
            .context(FrippyErrorKind::Tell)?;

        if let Some(users) = client.list_users(channel) {
            debug!("Outstanding tells for {:?}", receivers);

            for receiver in users
                .iter()
                .map(|u| u.get_nickname())
                .filter(|u| receivers.iter().any(|r| r == &u.to_lowercase()))
            {
                self.send_tells(client, receiver)?;
            }

            Ok(())
        } else {
            Ok(())
        }
    }

    fn send_tells(&self, client: &IrcClient, receiver: &str) -> Result<(), FrippyError> {
        if client.current_nickname() == receiver {
            return Ok(());
        }

        let mut tells = try_lock!(self.tells);

        let tell_messages = match tells.get_tells(&receiver.to_lowercase()) {
            Ok(t) => t,
            Err(e) => {
                // This warning only occurs if frippy is built without a database
                #[allow(unreachable_patterns)]
                return match e.kind() {
                    ErrorKind::NotFound => Ok(()),
                    _ => Err(e.context(FrippyErrorKind::Tell))?,
                };
            }
        };

        for tell in tell_messages {
            let now = Duration::new(time::now().to_timespec().sec as u64, 0);
            let dur = now - Duration::new(tell.time.timestamp() as u64, 0);
            let human_dur = format_duration(dur);

            client
                .send_notice(
                    receiver,
                    &format!(
                        "Tell from {} {} ago: {}",
                        tell.sender, human_dur, tell.message
                    ),
                )
                .context(FrippyErrorKind::Connection)?;

            debug!(
                "Sent {:?} from {:?} to {:?}",
                tell.message, tell.sender, receiver
            );
        }

        tells
            .delete_tells(&receiver.to_lowercase())
            .context(FrippyErrorKind::Tell)?;

        Ok(())
    }

    fn invalid_command(&self) -> &str {
        "Incorrect Command. \
         Send \"tell help\" for help."
    }

    fn help(&self) -> &str {
        "usage: {} tell user message\r\n\
         example: {0} tell Foobar Hello!"
    }
}

impl<T: Database> Plugin for Tell<T> {
    fn execute(&self, client: &IrcClient, message: &Message) -> ExecutionStatus {
        let res = match message.command {
            Command::JOIN(_, _, _) => self.send_tells(client, message.source_nickname().unwrap()),
            Command::NICK(ref nick) => self.send_tells(client, nick),
            Command::Response(resp, ref chan_info, _) => {
                if resp == Response::RPL_NAMREPLY {
                    debug!("NAMREPLY info: {:?}", chan_info);

                    self.on_namelist(client, &chan_info[chan_info.len() - 1])
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        };

        match res {
            Ok(_) => ExecutionStatus::Done,
            Err(e) => ExecutionStatus::Err(e),
        }
    }

    fn execute_threaded(&self, _: &IrcClient, _: &Message) -> Result<(), FrippyError> {
        panic!("Tell should not use threading")
    }

    fn command(&self, client: &IrcClient, command: PluginCommand) -> Result<(), FrippyError> {
        if command.tokens.is_empty() {
            return Ok(client
                .send_notice(&command.source, &self.invalid_command())
                .context(FrippyErrorKind::Connection)?);
        }

        let sender = command.source.to_owned();

        Ok(match command.tokens[0].as_ref() {
            "help" => client
                .send_notice(&command.source, &self.help())
                .context(FrippyErrorKind::Connection)
                .into(),
            _ => match self.tell_command(client, command) {
                Ok(msg) => client
                    .send_notice(&sender, &msg)
                    .context(FrippyErrorKind::Connection),
                Err(e) => client
                    .send_notice(&sender, &e.to_string())
                    .context(FrippyErrorKind::Connection)
                    .into(),
            },
        }?)
    }

    fn evaluate(&self, _: &IrcClient, _: PluginCommand) -> Result<String, String> {
        Err(String::from("This Plugin does not implement any commands."))
    }
}

use std::fmt;
impl<T: Database> fmt::Debug for Tell<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Tell {{ ... }}")
    }
}

pub mod error {
    #[derive(Copy, Clone, Eq, PartialEq, Debug, Fail, Error)]
    #[error = "TellError"]
    pub enum ErrorKind {
        /// Not found command error
        #[fail(display = "Tell was not found")]
        NotFound,

        /// MySQL error
        #[cfg(feature = "mysql")]
        #[fail(display = "Failed to execute MySQL Query")]
        MysqlError,

        /// No connection error
        #[cfg(feature = "mysql")]
        #[fail(display = "No connection to the database")]
        NoConnection,
    }
}
