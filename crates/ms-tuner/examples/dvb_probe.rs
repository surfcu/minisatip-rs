//! Hardware smoke test: tunes one Linux DVB adapter and reports TS throughput.
//!
//! `cargo run --example dvb_probe -- <adapter> <frontend> <msys> <freq_mhz> <pol> <src> <sr_ksps> [seconds]`
//! Append `--unicable=<slot>,<freq>,<pin>` or `--jess=<slot>,<freq>,<pin>` for single-cable LNBs.

#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn main() {
    use ms_tuner::linux_dvb::{DiseqcParams, SWITCH_JESS, SWITCH_UNICABLE};
    use ms_tuner::{DeliverySystem, Polarization, TuneParams};

    let mut positional = Vec::new();
    let mut diseqc = DiseqcParams::default();
    for arg in std::env::args().skip(1) {
        if let Some(spec) = arg.strip_prefix("--unicable=") {
            diseqc.switch_type = SWITCH_UNICABLE;
            set_user_band(&mut diseqc, spec);
        } else if let Some(spec) = arg.strip_prefix("--jess=") {
            diseqc.switch_type = SWITCH_JESS;
            set_user_band(&mut diseqc, spec);
        } else {
            positional.push(arg);
        }
    }
    if positional.len() < 7 {
        eprintln!(
            "usage: dvb_probe <adapter> <frontend> <dvbs|dvbs2|dvbt|dvbt2|dvbc> \
             <freq_mhz> <h|v|r|l> <src> <sr_ksps> [seconds] \
             [--unicable=slot,freq,pin] [--jess=slot,freq,pin]"
        );
        std::process::exit(2);
    }

    let msys = match positional[2].as_str() {
        "dvbs" => DeliverySystem::DvbS,
        "dvbs2" => DeliverySystem::DvbS2,
        "dvbt" => DeliverySystem::DvbT,
        "dvbt2" => DeliverySystem::DvbT2,
        "dvbc" => DeliverySystem::DvbC,
        other => fail(&format!("unknown delivery system '{other}'")),
    };
    let pol = match positional[4].as_str() {
        "h" => Polarization::Horizontal,
        "v" => Polarization::Vertical,
        "r" => Polarization::Right,
        "l" => Polarization::Left,
        other => fail(&format!("unknown polarisation '{other}'")),
    };
    let src: u8 = parse_number(&positional[5]);
    let params = TuneParams {
        freq: parse_number(&positional[3]),
        pol,
        msys,
        sr: parse_number(&positional[6]),
        src,
        src_specified: src > 0,
        ..TuneParams::default()
    };
    let seconds: u64 = positional.get(7).map_or(5, |s| parse_number(s));

    match run(
        parse_number(&positional[0]),
        parse_number(&positional[1]),
        diseqc,
        &params,
        seconds,
    ) {
        Ok(()) => {}
        Err(error) => fail(&error.to_string()),
    }
}

#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn run(
    adapter: u32,
    frontend: u32,
    diseqc: ms_tuner::linux_dvb::DiseqcParams,
    params: &ms_tuner::TuneParams,
    seconds: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io;
    use std::time::{Duration, Instant};

    use ms_tuner::linux_dvb::LinuxDvbTuner;
    use ms_tuner::{PidSet, TsBuf, Tuner, TunerError};

    let mut tuner = LinuxDvbTuner::open(adapter, frontend)?;
    tuner.set_diseqc_params(diseqc);
    tuner.tune(params)?;
    tuner.set_pids(&PidSet::from_pids([0, 16, 17]))?;

    let mut buf = TsBuf::with_packets(1024);
    let mut total = 0usize;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        match tuner.read(&mut buf) {
            Ok(0) => std::thread::sleep(Duration::from_millis(10)),
            Ok(count) => total += count,
            Err(TunerError::Io(error)) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
        buf.clear();
    }
    let status = tuner.status();
    println!(
        "read {total} bytes in {seconds}s, locked={} strength={} snr={}",
        status.locked, status.strength, status.snr
    );
    Ok(())
}

#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn set_user_band(diseqc: &mut ms_tuner::linux_dvb::DiseqcParams, spec: &str) {
    let mut parts = spec.split(',');
    diseqc.uslot = parts.next().map_or(0, parse_number);
    diseqc.ufreq = parts.next().map_or(0, parse_number);
    diseqc.pin = parts.next().map_or(0, parse_number);
}

#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn parse_number<T: std::str::FromStr>(value: &str) -> T {
    match value.parse() {
        Ok(number) => number,
        Err(_) => fail(&format!("invalid number '{value}'")),
    }
}

#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
fn fail(message: &str) -> ! {
    eprintln!("dvb_probe: {message}");
    std::process::exit(2);
}

#[cfg(not(all(feature = "linux-dvb", target_os = "linux")))]
fn main() {
    eprintln!("dvb_probe needs the linux-dvb feature on a Linux target");
    std::process::exit(1);
}
