#[cfg(feature = "mysql")]
extern crate dotenv;

use std::collections::HashMap;

#[cfg(feature = "mysql")]
use diesel::prelude::*;

#[cfg(feature = "mysql")]
use diesel::mysql::MysqlConnection;

use chrono::NaiveDateTime;

pub enum DbResponse {
    Success,
    Failed(&'static str),
}

#[cfg_attr(feature = "mysql", derive(Queryable))]
#[derive(Clone, Debug)]
pub struct Factoid {
    pub name: String,
    pub idx: i32,
    pub content: String,
    pub author: String,
    pub created: NaiveDateTime,
}

#[cfg_attr(feature = "mysql", derive(Insertable))]
#[cfg_attr(feature = "mysql", table_name="factoids")]
pub struct NewFactoid<'a> {
    pub name: &'a str,
    pub idx: i32,
    pub content: &'a str,
    pub author: &'a str,
    pub created: NaiveDateTime,
}


pub trait Database: Send {
    fn insert(&mut self, factoid: &NewFactoid) -> DbResponse;
    fn get(&self, name: &str, idx: i32) -> Option<Factoid>;
    fn count(&self, name: &str) -> Result<i32, &'static str>;
}

// HashMap
impl Database for HashMap<(String, i32), Factoid> {
    fn insert(&mut self, factoid: &NewFactoid) -> DbResponse {
        let factoid = Factoid {
            name: String::from(factoid.name),
            idx: factoid.idx,
            content: factoid.content.to_string(),
            author: factoid.author.to_string(),
            created: factoid.created,
        };

        let name = String::from(factoid.name.clone());
        match self.insert((name, factoid.idx), factoid) {
            None => DbResponse::Success,
            Some(_) => DbResponse::Failed("Factoid was overwritten"),
        }
    }

    fn get(&self, name: &str, idx: i32) -> Option<Factoid> {
        self.get(&(String::from(name), idx)).map(|f| f.clone())
    }

    fn count(&self, name: &str) -> Result<i32, &'static str> {
        Ok(self.iter()
               .filter(|&(&(ref n, _), _)| n == name)
               .count() as i32)
    }
}

// MySql
#[cfg(feature = "mysql")]
table! {
    factoids (name, idx) {
        name -> Varchar,
        idx -> Integer,
        content -> Text,
        author -> Varchar,
        created -> Timestamp,
    }
}

#[cfg(feature = "mysql")]
impl Database for MysqlConnection {
    fn insert(&mut self, factoid: &NewFactoid) -> DbResponse {
        use diesel;

        match diesel::insert_into(factoids::table)
                  .values(factoid)
                  .execute(self) {
            Ok(_) => DbResponse::Success,
            Err(_) => DbResponse::Failed("Database error - possible duplicate"),
        }
    }

    fn get(&self, name: &str, idx: i32) -> Option<Factoid> {
        factoids::table
            .find((name, idx))
            .limit(1)
            .load::<Factoid>(self)
            .ok()
            .and_then(|v| v.into_iter().next())
    }

    fn count(&self, name: &str) -> Result<i32, &'static str> {
        let count: Result<i64, _> = factoids::table
            .filter(factoids::columns::name.eq(name))
            .count()
            .first(self);

        match count {
            Ok(c) => Ok(c as i32),
            Err(_) => Err("Database Error"),
        }
    }
}
