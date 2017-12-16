extern crate rlua;

use std::fmt;
use std::str::FromStr;
use std::sync::Mutex;
use self::rlua::prelude::*;
use irc::client::prelude::*;
use irc::error::Error as IrcError;

use time;
use chrono::NaiveDateTime;

use plugin::*;
mod database;
use self::database::{Database, DbResponse};

static LUA_SANDBOX: &'static str = include_str!("sandbox.lua");

#[derive(PluginName)]
pub struct Factoids<T: Database> {
    factoids: Mutex<T>,
}

macro_rules! try_lock {
    ( $m:expr ) => {
        match $m.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl<T: Database> Factoids<T> {
    pub fn new(db: T) -> Factoids<T> {
        Factoids { factoids: Mutex::new(db) }
    }

    fn add(&self, server: &IrcServer, command: &mut PluginCommand) -> Result<(), IrcError> {
        if command.tokens.len() < 2 {
            return self.invalid_command(server, command);
        }

        let name = command.tokens.remove(0);
        let content = command.tokens.join(" ");
        let count = match try_lock!(self.factoids).count(&name) {
            Ok(c) => c,
            Err(e) => return server.send_notice(&command.source, e),
        };

        let tm = time::now().to_timespec();

        let factoid = database::NewFactoid {
            name: &name,
            idx: count,
            content: &content,
            author: &command.source,
            created: NaiveDateTime::from_timestamp(tm.sec, tm.nsec as u32),
        };

        match try_lock!(self.factoids).insert(&factoid) {
            DbResponse::Success => server.send_notice(&command.source, "Successfully added"),
            DbResponse::Failed(e) => server.send_notice(&command.source, &e),
        }
    }

    fn get(&self, server: &IrcServer, command: &PluginCommand) -> Result<(), IrcError> {

        let (name, idx) = match command.tokens.len() {
            0 => return self.invalid_command(server, command),
            1 => {
                let name = &command.tokens[0];
                let count = match try_lock!(self.factoids).count(name) {
                    Ok(c) => c,
                    Err(e) => return server.send_notice(&command.source, e),
                };

                if count < 1 {
                    return server.send_notice(&command.source, &format!("{} does not exist", name));
                }

                (name, count - 1)
            }
            _ => {
                let name = &command.tokens[0];
                let idx = match i32::from_str(&command.tokens[1]) {
                    Ok(i) => i,
                    Err(_) => return server.send_notice(&command.source, "Invalid index"),
                };

                (name, idx)
            }
        };

        let factoid = match try_lock!(self.factoids).get(name, idx) {
            Some(v) => v,
            None => return server.send_notice(&command.source, &format!("{}~{} does not exist", name, idx)),
        };

        server.send_privmsg(&command.target,
                            &format!("{}: {}", factoid.name, factoid.content))
    }

    fn info(&self, server: &IrcServer, command: &PluginCommand) -> Result<(), IrcError> {

        match command.tokens.len() {
            0 => self.invalid_command(server, command),
            1 => {
                let name = &command.tokens[0];
                let count = match try_lock!(self.factoids).count(name) {
                    Ok(c) => c,
                    Err(e) => return server.send_notice(&command.source, e),
                };

                match count {
                    0 => server.send_notice(&command.source, &format!("{} does not exist", name)),
                    1 => {
                        server.send_privmsg(&command.target,
                                            &format!("There is 1 version of {}", name))
                    }
                    _ => {
                        server.send_privmsg(&command.target,
                                            &format!("There are {} versions of {}", count, name))
                    }
                }
            }
            _ => {
                let name = &command.tokens[0];
                let idx = match i32::from_str(&command.tokens[1]) {
                    Ok(i) => i,
                    Err(_) => return server.send_notice(&command.source, "Invalid index"),
                };

                let factoid = match try_lock!(self.factoids).get(name, idx) {
                    Some(v) => v,
                    None => {
                        return server.send_notice(&command.source,
                                                  &format!("{}~{} does not exist", name, idx))
                    }
                };

                server.send_privmsg(&command.target,
                                    &format!("{}: Added by {} at {} UTC",
                                             name,
                                             factoid.author,
                                             factoid.created))
            }

        }
    }

    fn exec(&self,
            server: &IrcServer,
            mut command: PluginCommand,
            error: bool)
            -> Result<(), IrcError> {
        if command.tokens.len() < 1 {
            self.invalid_command(server, &command)

        } else {
            let name = command.tokens.remove(0);
            let count = match try_lock!(self.factoids).count(&name) {
                Ok(c) => c,
                Err(e) => return server.send_notice(&command.source, e),
            };

            let factoid = match try_lock!(self.factoids).get(&name, count - 1) {
                Some(v) => v.content,
                None if error => return self.invalid_command(server, &command),
                None => return Ok(()),
            };

            let value = &if factoid.starts_with(">") {
                let factoid = String::from(&factoid[1..]);

                if factoid.starts_with(">") {
                    factoid
                } else {
                    match self.run_lua(&name, &factoid, &command) {
                        Ok(v) => v,
                        Err(e) => format!("{}", e),
                    }
                }
            } else {
                factoid
            };

            server.send_privmsg(&command.target, &value)
        }
    }

    fn run_lua(&self,
               name: &str,
               code: &str,
               command: &PluginCommand)
               -> Result<String, rlua::Error> {

        let args = command
            .tokens
            .iter()
            .filter(|x| !x.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<String>>();

        let lua = Lua::new();
        let globals = lua.globals();

        globals.set("factoid", lua.load(code, Some(name))?)?;
        globals.set("args", args)?;
        globals.set("input", command.tokens.join(" "))?;
        globals.set("user", command.source.clone())?;
        globals.set("channel", command.target.clone())?;
        globals.set("output", lua.create_table())?;

        lua.exec::<()>(LUA_SANDBOX, Some(name))?;
        let output: Vec<String> = globals.get::<_, Vec<String>>("output")?;

        Ok(output.join("|").replace("\n", "|"))
    }

    fn invalid_command(&self, server: &IrcServer, command: &PluginCommand) -> Result<(), IrcError> {
        server.send_notice(&command.source, "Invalid Command")
    }
}

impl<T: Database> Plugin for Factoids<T> {
    fn is_allowed(&self, _: &IrcServer, message: &Message) -> bool {
        match message.command {
            Command::PRIVMSG(_, ref content) => content.starts_with('!'),
            _ => false,
        }
    }

    fn execute(&self, server: &IrcServer, message: &Message) -> Result<(), IrcError> {
        if let Command::PRIVMSG(_, mut content) = message.command.clone() {
            content.remove(0);

            let t: Vec<String> = content.split(' ').map(ToOwned::to_owned).collect();

            let c = PluginCommand {
                source: message.source_nickname().unwrap().to_string(),
                target: message.response_target().unwrap().to_string(),
                tokens: t,
            };

            self.exec(server, c, false)

        } else {
            Ok(())
        }
    }

    fn command(&self, server: &IrcServer, mut command: PluginCommand) -> Result<(), IrcError> {
        if command.tokens.is_empty() {
            return self.invalid_command(server, &command);
        }

        let sub_command = command.tokens.remove(0);
        match sub_command.as_ref() {
            "add" => self.add(server, &mut command),
            "get" => self.get(server, &command),
            "info" => self.info(server, &command),
            "exec" => self.exec(server, command, true),
            _ => self.invalid_command(server, &command),
        }
    }
}

impl<T: Database> fmt::Debug for Factoids<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Factoids {{ ... }}")
    }
}
