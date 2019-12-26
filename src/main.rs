use std::fs::File;
use std::str::FromStr;
use std::time::{Duration, Instant};

// CPU
// Temp
// Free space
// Internet
// Battery
// Date
// Sound

trait ReadStr {
    fn read_str<'a>(&mut self, buf: &'a mut [u8]) -> std::io::Result<&'a str>;
}

impl<T> ReadStr for T
where
    T: std::os::unix::io::AsRawFd,
{
    fn read_str<'a>(&mut self, buf: &'a mut [u8]) -> std::io::Result<&'a str> {
        let len = unsafe {
            let ret = libc::pread64(
                self.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
            );
            if ret < 0 {
                return Err(std::io::Error::last_os_error());
            } else {
                ret as usize
            }
        };
        Ok(std::str::from_utf8(&buf[..len]).unwrap())
    }
}

fn open_cpu_temperatures_file() -> std::io::Result<File> {
    for device in std::fs::read_dir("/sys/class/hwmon")?.filter_map(Result::ok) {
        let path = device.path();
        let name_path = path.join("name");
        if std::fs::read_to_string(name_path)
            .unwrap_or_default()
            .trim()
            == "coretemp"
        {
            let mut temps = std::fs::read_dir(&path)?
                .filter_map(Result::ok)
                .filter(|e| e.path().to_str().unwrap_or_default().contains("_input"))
                .collect::<Vec<_>>();
            temps.sort_by_key(|e| e.path());
            return File::open(&temps[0].path());
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "No /sys/class/hwmon named coretemp found",
    ))
}

struct NetworkDevice {
    name: String,
    operstate: File,
}

fn open_battery_file() -> Result<Battery, Box<dyn std::error::Error>> {
    let charge_full_design =
        std::fs::read_to_string("/sys/class/power_supply/BAT0/charge_full_design")?
            .trim()
            .parse::<f64>()
            .unwrap();
    Ok(Battery {
        charge_full_design,
        charge_now: File::open("/sys/class/power_supply/BAT0/charge_now")?,
    })
}

struct Battery {
    charge_full_design: f64,
    charge_now: File,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut cpu_stats = File::open("/proc/stat").unwrap();
    let mut cpu_temp = open_cpu_temperatures_file().unwrap();
    let mut battery = open_battery_file().ok();

    let desired_network_devices = std::env::args().skip(1).collect::<Vec<_>>();
    let mut network_devices = Vec::new();
    for name in &desired_network_devices {
        if let Ok(f) = File::open(format!("/sys/class/net/{}/operstate", name)) {
            network_devices.push(NetworkDevice {
                name: name.clone(),
                operstate: f,
            })
        }
    }

    let mut buf = [0u8; 1024];

    let mut cpu_fields: Vec<u64> = Vec::new();
    let mut previous_cpu_fields = cpu_fields.clone();

    cpu_fields.extend(
        cpu_stats
            .read_str(&mut buf)?
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .skip(1)
            .filter(|s| !s.is_empty())
            .map(|s| u64::from_str(s).unwrap()),
    );

    print!("{{\"version\":1}}[");
    loop {
        let start = Instant::now();

        previous_cpu_fields.clear();
        previous_cpu_fields.extend(&cpu_fields);

        cpu_fields.clear();
        cpu_fields.extend(
            cpu_stats
                .read_str(&mut buf)?
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .skip(1)
                .filter(|s| !s.is_empty())
                .map(|s| u64::from_str(s).unwrap()),
        );

        let cycles_elapsed =
            cpu_fields.iter().sum::<u64>() - previous_cpu_fields.iter().sum::<u64>();
        let idle_cycles = cpu_fields[3] - previous_cpu_fields[3];

        let cpu_utilization = 100.0 * (1.0 - (idle_cycles as f64 / cycles_elapsed as f64));

        print!("[");
        print!("{{\"full_text\":\"{:02.0}%\"}},", cpu_utilization);

        let temp_bytes = cpu_temp.read_str(&mut buf)?;

        print!(
            "{{\"full_text\":\"{}°C\"}},",
            u64::from_str(temp_bytes.trim()).unwrap() / 1000
        );

        let free_space = unsafe {
            let mut stats: libc::statfs64 = core::mem::zeroed();
            libc::statfs64(
                "/\0".as_ptr() as *const libc::c_char,
                &mut stats as *mut libc::statfs64,
            );
            stats.f_bavail * stats.f_bsize as u64
        };

        print!(
            "{{\"full_text\":\"{:.0} GiB\"}},",
            free_space as f64 / 1024. / 1024. / 1024.
        );

        for device in &mut network_devices {
            let state = device.operstate.read_str(&mut buf)?.trim();
            if state == "up" {
                print!(
                    "{{\"color\":\"#00ff00\",\"full_text\":\"{}: up\"}},",
                    device.name,
                );
            } else {
                print!(
                    "{{\"color\":\"#ff0000\",\"full_text\":\"{}: {}\"}},",
                    device.name, state
                );
            }
        }

        if let Some(battery) = battery.as_mut() {
            if let Ok(text) = battery.charge_now.read_str(&mut buf) {
                let charge_now = text.trim().parse::<f64>().unwrap();
                print!(
                    "{{\"full_text\":\"BAT {:.2}%\"}},",
                    charge_now / battery.charge_full_design * 100.0
                );
            }
        }

        let tm = unsafe {
            let time = libc::time(core::ptr::null_mut());
            let mut localtime: libc::tm = core::mem::zeroed();
            libc::localtime_r(
                &time as *const libc::time_t,
                &mut localtime as *mut libc::tm,
            );
            localtime
        };

        let days = ["Sun", "Mon", "Tue", "Wed", "Thur", "Fri", "Sat"];
        print!(
            "{{\"full_text\":\"{:04}-{:02}-{:02} {} {:02}:{:02}:{:02}\"}}",
            tm.tm_year + 1900,
            tm.tm_mon,
            tm.tm_mday,
            days.get(tm.tm_wday as usize).unwrap_or(&"???"),
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        );

        println!("],");

        if let Some(sleep_duration) = Duration::from_secs(1).checked_sub(start.elapsed()) {
            std::thread::sleep(sleep_duration);
        }
    }
}
