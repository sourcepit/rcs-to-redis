#[macro_use]
extern crate clap;
#[macro_use]
extern crate common_failures;
extern crate caps;
#[macro_use]
extern crate failure;
extern crate libc;
extern crate redis;
extern crate thread_priority;

mod assert;
mod gpio;
mod rcs;
mod redis_streams;

use common_failures::prelude::*;

use assert::assert;
use caps::CapSet;
use caps::Capability;
use clap::App;
use clap::Arg;
use rcs::RemoteControl;
use rcs::Switch;
use rcs::SwitchCode::SwitchB;
use rcs::SwitchState::Off;
use rcs::SwitchState::On;
use redis::Commands;
use redis::Value;
use redis_streams::as_bulk;
use redis_streams::as_status;
use redis_streams::as_stream_entry;
use redis_streams::as_string;
use redis_streams::is_nil;
use redis_streams::EntryId;
use std::collections::HashMap;
use std::thread;
use std::time::Duration;
use thread_priority::set_thread_priority;
use thread_priority::thread_native_id;
use thread_priority::RealtimeThreadSchedulePolicy;
use thread_priority::ThreadPriority;
use thread_priority::ThreadSchedulePolicy;

const ARG_REDIS_HOST: &str = "redis-host";
const ARG_REDIS_PORT: &str = "redis-port";
const ARG_NAME: &str = "name";
const ARG_GPIO_PIN: &str = "gpio-pin";

fn try_upgrade_thread_priority() -> Result<()> {
    let has_cap_sys_nice = match caps::has_cap(None, CapSet::Permitted, Capability::CAP_SYS_NICE) {
        Ok(v) => v,
        Err(e) => return Err(format_err!("{}", e)),
    };
    if has_cap_sys_nice {
        let thread_id = thread_native_id();
        let res = set_thread_priority(
            thread_id,
            ThreadPriority::Max,
            ThreadSchedulePolicy::Realtime(RealtimeThreadSchedulePolicy::Fifo),
        );
        match res {
            Ok(_) => {}
            Err(e) => return Err(format_err!("{:?}", e)),
        }
    };
    Ok(())
}

fn run() -> Result<()> {
    let args = App::new(crate_name!())
        .version(crate_version!())
        .author(crate_authors!())
        .arg(
            Arg::with_name(ARG_REDIS_HOST)
                .long(ARG_REDIS_HOST)
                .multiple(false)
                .takes_value(true)
                .required(false)
                .default_value("localhost"),
        )
        .arg(
            Arg::with_name(ARG_REDIS_PORT)
                .long(ARG_REDIS_PORT)
                .multiple(false)
                .takes_value(true)
                .required(false)
                .default_value("6379"),
        )
        .arg(
            Arg::with_name(ARG_NAME)
                .long(ARG_NAME)
                .multiple(false)
                .takes_value(true)
                .required(false)
                .default_value("rcs"),
        )
        .arg(
            Arg::with_name(ARG_GPIO_PIN)
                .long(ARG_GPIO_PIN)
                .multiple(false)
                .takes_value(true)
                .required(false)
                .default_value("17"),
        )
        .get_matches();

    let redis_host = value_t!(args, ARG_REDIS_HOST, String)?;
    let redis_port = value_t!(args, ARG_REDIS_PORT, usize)?;
    let name = value_t!(args, ARG_NAME, String)?;
    let gpio_pin = value_t!(args, ARG_GPIO_PIN, usize)?;

    // requires:
    // sudo setcap cap_sys_nice=ep <file>
    try_upgrade_thread_priority()?;

    let mut redis_connection =
        redis::Client::open(format!("redis://{}:{}", redis_host, redis_port))?.get_connection()?;

    let stream_name = name.clone();
    let start_key = format!("{}_start", name);

    let mut start = match redis_connection.get::<&String, Option<String>>(&start_key)? {
        Some(start) => EntryId::from_str(start)?,
        None => EntryId::new(0, 0),
    };

    loop {
        println!("{}", start);

        let streams: Value = redis::cmd("XREAD")
            .arg("BLOCK")
            .arg("5000")
            .arg("STREAMS")
            .arg(&stream_name)
            .arg(start.to_string())
            .query(&mut redis_connection)?;

        // nil == timeout
        if !is_nil(&streams) {
            let mut streams = as_bulk(streams)?;
            assert(
                || streams.len() == 1,
                format!("We query only for one stream, but got: {:?}", streams),
            )?;

            // extract rcs stream
            let mut stream_name_to_entries = as_bulk(streams.remove(0))?;
            // extract rcs stream entries. 0 == name, 1 == entries
            let stream_entries = as_bulk(stream_name_to_entries.remove(1))?;

            for stream_entry in stream_entries {
                let stream_entry = as_stream_entry(stream_entry)?;
                let entry_id = stream_entry.0;
                let entry_values = stream_entry.1;
                println!("{}: {:?}", entry_id, entry_values);
                start = entry_id.next();
            }

            redis_connection.set(&start_key, start.to_string())?;
        }
    }

    let system_code = [true, true, true, false, false];

    let switch_b = Switch::new(&system_code, SwitchB);

    let mut rc = RemoteControl::open(gpio_pin)?;

    rc.send(&switch_b, On);
    thread::sleep(Duration::from_secs(2));

    rc.send(&switch_b, Off);
    thread::sleep(Duration::from_secs(2));

    Ok(())
}

quick_main!(run);
