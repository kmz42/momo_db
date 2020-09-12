#![feature(proc_macro_hygiene, decl_macro)]

#[macro_use] extern crate rocket;
extern crate postgres;
extern crate serde;
extern crate num;
extern crate rand;
extern crate chrono;

use chrono::NaiveDateTime;
use postgres::{Connection, TlsMode};
use std::f64;
use rand::{Rng, thread_rng};
use serde::{Deserialize, Serialize};
use std::os::unix::fs;
use std::fs as stdfs;


struct Identity {
    id: i64,
    balance: f64,
    d_id: Option<i64>, //discord id if the identity has one
    pkey: Option<String> //public key if the identity has one
}

#[derive(Serialize, Deserialize, Copy, Clone)]
enum EventType {
    Gambling, Tipped, Tipping, Claiming, Investing, Liquidating
}

#[derive(Serialize, Deserialize)]
struct EventDataPoint {
    delta: f64,
    balance: f64,
    time: NaiveDateTime,
    eventtype: EventType
}

struct AuthInfo {
    api_key: i64
}

/**
 * IdentityService. TODO: refactor to another file
 */

fn is_authorized(auth: &AuthInfo, conn : &Connection) -> bool {
    let rows = &conn.query("select * from api_key where key = $1", &[&auth.api_key]).unwrap();
    if rows.len() == 0 {
        false
    } else {
        true
    }
}

// Takes a transaction and determines what event it is based on the IDs involved
fn determine_event_type(from_id: i64, to_id: i64, uid: i64) -> EventType {
    // 10 is Mimi
    // sending money to mimi means you're gambling (positive or negative)
    if to_id == 10 {
        return EventType::Gambling;
    }

    // 22 is Ottobot
    // sending money to ottobot means you are investing
    // receiving money from ottobot means you are liquidating
    if from_id == 22 {
        return EventType::Liquidating;
    }
    if to_id == 22 {
        return EventType::Investing;
    }

    if from_id == uid {
        return EventType::Tipping;
    }

    if to_id == uid {
        return EventType::Tipped;
    }

    // Hopefully this should be impossible
    println!("Encountered a transaction that makes no sense with uid {}", uid);
    panic!("Illegal!");
}

// Gets all transactions for a user sorted reverse by time
fn get_tx_for_user(id: i64, conn: &Connection) -> std::result::Result<postgres::rows::Rows, postgres::Error> {
    let rows = conn.query("select * from itl_tx_log where u1_id = $1 or u2_id = $1 order by tx_time asc", &[&id]);
    return rows;
}

// Gets all handouts for a user sorted reverse by time
fn get_handouts_for_user(id: i64, conn: &Connection) -> std::result::Result<postgres::rows::Rows, postgres::Error> {
    let rows = conn.query("select * from itl_handout_log where u_id = $1 order by tx_time asc", &[&id]);
    return rows;
}

// Loads identity associated with the discord id. If none exists, create one.
fn load_from_did(d_id: i64, conn: &Connection) -> Identity {
  let rows = &conn.query("select * from wlt_id cross join discord_user where wlt_id.id = discord_user.wlt_id and discord_user.id = $1", &[&d_id]).unwrap();
  let mut id: i64;
  let mut balance: f64;
  let mut pkey: Option<String> = None;
  if rows.len() == 0 {
      println!("Creating new identity for discord id {}", d_id);
    // TODO make new
      let rows2 = &conn.query("insert into wlt_id (momo_bal) values (0.0) returning id", &[]).unwrap();
      let row = rows2.get(0);
      id = row.get(0);
      // TODO check for error on "successful" and freak out or something
      let successful = &conn.execute("insert into discord_user (id, wlt_id) values ($1, $2)", &[&d_id, &id]);
      balance = 0.0;
  } else {
      let row = rows.get(0);
      id = row.get(0);
      balance = row.get(1);

      // Check for the pkey
      for row in &conn.query("select pubkey from pubkey_user where wlt_id=$1", &[&id]).unwrap(){
          pkey = row.get(0);
      }
  }
  Identity{
      id : id,
      balance: balance,
      d_id: Some(d_id),
      pkey: pkey
  }
}

fn log_itl_tx(ident_one: &Identity, ident_two: &Identity, delta: &f64, conn: &Connection) {
    &conn.execute("insert into itl_tx_log (u1_id, u2_id, delta) values ($1, $2, $3)", &[&ident_one.id,&ident_two.id,&delta]);
}

fn log_coin_creation(ident: &Identity, delta: &f64, conn: &Connection) {
    &conn.execute("insert into itl_handout_log (u_id, delta) values ($1, $2)", &[&ident.id, &delta]);
}

fn has_daily_handout(uid: i64, conn: &Connection) -> bool {
    let rows = &conn.query("select count(*) from itl_handout_log where u_id = $1 and tx_time >= current_date::timestamp", &[&uid]).unwrap();
    let val : i64 = rows.get(0).get(0);
    if val == 0 {
        false
    } else {
        true
    }
}
    

fn update_balance(ident: &Identity, conn: &Connection, new_balance: f64) {
    &conn.execute("update wlt_id set momo_bal = $1 where id = $2", &[&new_balance, &ident.id]).unwrap();
}

fn get_top(conn : &Connection, num: i64) -> Vec<Identity> {
    let rows = &conn.query("select w.momo_bal, d.id from discord_user as d, wlt_id as w where d.wlt_id = w.id order by momo_bal desc limit $1", &[&num]).unwrap();
    // Make a bunch of identity objects that only have discord_id and momo_bal set
    let mapped = rows.iter().map(|row| Identity{ id: 0, balance: row.get(0), d_id : row.get(1), pkey: None }).collect();
    return mapped;
}

// Logs the transaction and updates the house's coins
fn run_gambling_updates(user_ident: &Identity, conn: &Connection, delta: &f64) {
   // negate delta to get the house's delta
   let real_delta = -1.0 * delta;

   // query the houses' current identity & balance
   let house_identity : Identity = load_from_did(401763697300865036, &conn); // mimibots' ID

   // update the houses' balance
   let new_house_balance = house_identity.balance + real_delta;
   update_balance(&house_identity, &conn, new_house_balance);

   // log tx
   log_itl_tx(&user_ident, &house_identity, &real_delta, &conn);
}

/**
 * End of IdentityService
 */

// get balance by discord id
#[get("/discord/<id>")]
fn balance_by_id(id: i64) -> String {
    let conn = Connection::connect("postgres://postgres:test@localhost:5432/momo", TlsMode::None).unwrap();
    let ident: Identity = load_from_did(id, &conn);
    format!("{{ \"balance\": {}}}", ident.balance)
}

// gets the <num> richest users on discord by their discord ID
#[get("/discord/standings/<num>")]
fn get_top_standings(num: i64) -> String {
    let conn = Connection::connect("postgres://postgres:test@localhost:5432/momo", TlsMode::None).unwrap();
    let top = get_top(&conn, num);
    let mapped : Vec<String> = top.iter().map(|ident| format!("{{ \"id\": {}, \"balance\": {} }}", ident.d_id.unwrap(), ident.balance)).collect();
    let mut outstr = format!("{}", mapped[0]);
    for x in 1..mapped.len() {
        outstr = format!("{}, {}", outstr, mapped[x]);
    }
    return format!("[ {} ]", outstr);

}

// Alter balance by discord id by amount delta.
// Returns the remaining balance on the account
#[post("/discord/createcoin/<id>/<delta>")]
fn add_by_id(id: i64, delta: f64) -> String{
    let conn = Connection::connect("postgres://postgres:test@localhost:5432/momo", TlsMode::None).unwrap();
    let ident: Identity = load_from_did(id, &conn);
    let new_balance : f64 = ident.balance + delta;
    // NOT SURE IF WANT TO LOG log_coin_creation(&ident, &delta, &conn);
    update_balance(&ident, &conn, new_balance);
    format!("Success")
}

fn discover_coin() -> f64 {
    let mut rng = thread_rng();
    let x: f64 = rng.gen();
    if x < 0.01 {
        return 100.0;
    }
    if x < 0.05 {
        return 5.0;
    }
    if x < 0.3 {
        return 2.0;
    }
    if x > 0.95 {
        return 0.3;
    }
    if x > 0.99 {
        return 0.001;
    }
    return 1.0; 
}

// Adds one coin to balance. Only works once per day.
#[post("/discord/claimcoin/<id>")]
fn claim_free_coin(id: i64) -> String{
    let conn = Connection::connect("postgres://postgres:test@localhost:5432/momo", TlsMode::None).unwrap();
    let ident: Identity = load_from_did(id, &conn);
    let can_get_coin = !has_daily_handout(ident.id, &conn);
    if can_get_coin {
        let discovery_amt = discover_coin();
        let new_balance : f64 = ident.balance + discovery_amt;
        log_coin_creation(&ident, &discovery_amt, &conn);
        update_balance(&ident, &conn, new_balance);
        format!("{{ \"balance\" : {}, \"delta\" : {}}}", new_balance, &discovery_amt)
    } else {
        format!("Coin already claimed in the past day")
    }
}

// Gambles double-or-nothing
#[post("/discord/gamble/<id>/<bet>/<p>")]
fn double_or_nothing(id: i64, bet: f64, p: f64) -> String { // lul add auth
    let conn = Connection::connect("postgres://postgres:test@localhost:5432/momo", TlsMode::None).unwrap();
    let ident: Identity = load_from_did(id, &conn);
    if ident.balance < bet {
        return format!("YO you can't just bet money you don't have!!");
    }
    if bet < 0.0 {
        return format!("Try being more positive");
    }
    if p < 0.0 || p > 1.0 {
        return format!("Must have a probability between 0.0 and 1.0");
    }
    let mut rng = thread_rng();
    let x: f64 = rng.gen();
    let mut new_bal = ident.balance;
    let mut status = "";
    let mut delta : f64 = 0.0;
    if x < p { // win
        delta = bet / p - bet;
        new_bal = ident.balance + delta;
        status = "win"
    } else {
        delta = -1.0 * bet;
        new_bal = ident.balance + delta;
        status = "lose"
    }
    update_balance(&ident, &conn, new_bal);
    run_gambling_updates(&ident, &conn, &delta);
    return format!("{{ \"win\" : \"{}\", \"balance\": {}}}", status, new_bal );
}

#[post("/discord/slots/<id>/<bet>")]
fn run_slot_machine(id: i64, bet: f64) -> String {
    let conn = Connection::connect("postgres://postgres:test@localhost:5432/momo", TlsMode::None).unwrap();
    let ident: Identity = load_from_did(id, &conn);
    if ident.balance < bet {
        return format!("YO you can't just bet money you don't have!!");
    }
    if bet < 0.0 {
        return format!("Try being more positive");
    }

    // slot machine is fixed at a 1% chance of success
    let p = 0.01;

    let mut rng = thread_rng();
    let x: f64 = rng.gen();
    let mut new_bal = ident.balance;
    let mut status = "";
    let mut delta : f64 = 0.0;
    let multiplier: f64;
    // expected return is positive! exact amount based on the bet
    if bet < 1.0 {
        multiplier = 102.0;
    } else if bet < 10.0 {
        multiplier = 104.0;
    } else {
        multiplier = 108.0;
    }

    if x < p { // win
        delta = bet * multiplier;
        new_bal = ident.balance + delta;
        status = "win"
    } else {
        delta = -1.0 * bet;
        new_bal = ident.balance + delta;
        status = "lose"
    }

    update_balance(&ident, &conn, new_bal);
    run_gambling_updates(&ident, &conn, &delta);
    return format!("{{ \"win\" : \"{}\", \"return\" : \"{}\" \"balance\": {}}}", status, delta, new_bal );
}

fn copy_with_balance(orig: &EventDataPoint, balance: f64) -> EventDataPoint {
    return EventDataPoint {
        delta: orig.delta,
        balance: balance,
        time: orig.time,
        eventtype: orig.eventtype
    };
}

// Builds a graph for input user
#[post("/discord/buildgraph/<id>")]
fn build_graph_data(id: i64) -> String {
    let conn = Connection::connect("postgres://postgres:test@localhost:5432/momo", TlsMode::None).unwrap();
    let ident: Identity = load_from_did(id, &conn);
    let cur_bal = ident.balance;

    let mut transaction_data: Vec<EventDataPoint> = Vec::new();
    let tx_rows = get_tx_for_user(ident.id, &conn);
    for row in &tx_rows.unwrap() {

        let mut nextData = EventDataPoint {
            delta: row.get("delta"),
            balance: 0.0,
            time: row.get("tx_time"),
            eventtype: determine_event_type(row.get("u1_id"), row.get("u2_id"), ident.id)
        };
        transaction_data.push(nextData);
    }

    let handout_rows = get_handouts_for_user(ident.id, &conn);
    let mut handout_data: Vec<EventDataPoint> = Vec::new();
    
    for row in &handout_rows.unwrap() {
        let nextData = EventDataPoint {
            time: row.get("tx_time"),
            balance: 0.0,
            delta: row.get("delta"),
            eventtype: EventType::Claiming
        };
        handout_data.push(nextData);
    }
    
    // Build a finalized array with a running total combining the two arrays
    let mut final_data: Vec<EventDataPoint> = Vec::new();
    let mut running_balance = cur_bal;
    while transaction_data.len() > 0 || handout_data.len() > 0 {
        if transaction_data.len() == 0 {
            let next_data = handout_data.pop().unwrap();
            final_data.push(copy_with_balance(&next_data, running_balance));
            running_balance = running_balance - next_data.delta;
            continue;
        }

        if handout_data.len() == 0 {
            let next_data = transaction_data.pop().unwrap();
            final_data.push(copy_with_balance(&next_data, running_balance));
            running_balance = running_balance - next_data.delta;
            continue;
        }

        // Pop the most recent of the last items of either array
        let nextData: EventDataPoint;
        let mut truthValue = false;
        {
            let nextTransactionData: &EventDataPoint = &(transaction_data[transaction_data.len()-1]);
            let nextHandoutData: &EventDataPoint = &handout_data[handout_data.len()-1];

            if nextTransactionData.time > nextHandoutData.time {
                nextData = copy_with_balance(nextTransactionData, running_balance);
                // money losing activities get factor of -1 times the delta
                let mut factor = match nextTransactionData.eventtype {
                    EventType::Tipping | EventType::Investing | EventType::Gambling => -1.0,
                    _ => 1.0
                };
                running_balance = running_balance - factor * nextTransactionData.delta;
                final_data.push(nextData);
                //transaction_data.pop();
                truthValue = true; 
            } else {
                nextData = copy_with_balance(nextHandoutData, running_balance);
                running_balance = running_balance - nextHandoutData.delta;
                final_data.push(nextData);
                //handout_data.pop();
                truthValue = false;
            }
        }

        if truthValue {
            transaction_data.pop();
        } else {
            handout_data.pop();
        } 

    }


    // Write final data as a CSV thing to a file somewhere
    final_data.reverse();
    let serialized = serde_json::to_string(&final_data).unwrap();
    stdfs::create_dir(format!("/var/www/discorduser/{}", ident.id));
    
    
    stdfs::remove_file(format!("/var/www/discorduser/{}/data.json", ident.id));
    stdfs::write(format!("/var/www/discorduser/{}/data.json", ident.id),serialized);
    fs::symlink("/var/www/discorduser/display.html", format!("/var/www/discorduser/{}/display.html", ident.id));
    fs::symlink("/var/www/discorduser/graphit.js", format!("/var/www/discorduser/{}/graphit.js", ident.id));
    return format!("Done, see http://momobot.net/discorduser/{}/display.html",ident.id);
}


// Tips from one user to another by amount delta, which should be positive and not exceed the from_users' balance
#[post("/discord/tip/<from_id>/<to_id>/<delta>")]
fn tip_user(from_id: i64, to_id: i64, delta: f64) -> String {
    if (&delta).is_nan() {
        return format!("Woah, I'm like tripping out dude")
    }
    let conn = Connection::connect("postgres://postgres:test@localhost:5432/momo", TlsMode::None).unwrap();
    if &from_id == &to_id {
        return format!("Nope that won't work")
    }
    if (&from_id < &0) || (&to_id < &0) {
        return format!("WTF IS THIS ID")
    }
    if &delta < &0.0 {
        return format!("Har har")
    }
    let from_ident: Identity = load_from_did(from_id, &conn);
    let to_ident: Identity = load_from_did(to_id, &conn);
    if &from_ident.balance < &delta {
        format!("You have insufficient funds")
    } else {
        // successful internal tx
        log_itl_tx(&from_ident, &to_ident, &delta, &conn);
        let new_from_balance : f64 = from_ident.balance - delta;
        update_balance(&from_ident, &conn, new_from_balance);
        let new_to_balance : f64 = to_ident.balance + delta;
        update_balance(&to_ident, &conn, new_to_balance);
        format!("{{ \"balance\" : {}}}", new_from_balance)
    }
}

fn main() {
    rocket::ignite().mount("/wallet", routes![balance_by_id, tip_user, add_by_id, claim_free_coin, double_or_nothing, get_top_standings, build_graph_data]).launch();
}
