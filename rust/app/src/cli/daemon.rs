use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct PidRecord {
    pub pid: u32,
    pub port: u16,
    pub started_at: String,
    pub exe: String,
}

pub struct DaemonManager {
    run_dir: PathBuf,
    pid_file: PathBuf,
    log_file: PathBuf,
}

impl DaemonManager {
    pub fn new() -> Self {
        let base_dir = std::env::var("DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                PathBuf::from(home).join(".9router")
            });
        let run_dir = base_dir.join("run");
        let pid_file = run_dir.join("gateway.pid");
        let log_file = run_dir.join("gateway.log");

        Self {
            run_dir,
            pid_file,
            log_file,
        }
    }

    pub fn is_pid_alive(pid: u32) -> bool {
        #[cfg(unix)]
        {
            Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
        #[cfg(windows)]
        {
            Command::new("tasklist")
                .args(["/FI", &format!("PID eq {}", pid), "/NH"])
                .output()
                .map(|out| {
                    let s = String::from_utf8_lossy(&out.stdout);
                    s.contains(&pid.to_string())
                })
                .unwrap_or(false)
        }
    }

    pub fn read_pid_record(&self) -> Option<PidRecord> {
        if !self.pid_file.exists() {
            return None;
        }
        let content = fs::read_to_string(&self.pid_file).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn write_pid_record(&self, record: &PidRecord) -> anyhow::Result<()> {
        fs::create_dir_all(&self.run_dir)?;
        let json = serde_json::to_string_pretty(record)?;
        fs::write(&self.pid_file, json)?;
        Ok(())
    }

    fn remove_pid_file(&self) {
        let _ = fs::remove_file(&self.pid_file);
    }

    pub fn start(&self, daemon: bool, host: Option<String>, port: Option<u16>) -> anyhow::Result<()> {
        let port = port.unwrap_or_else(|| {
            std::env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(20127)
        });
        let host_str = host.unwrap_or_else(|| "0.0.0.0".to_string());

        // Check if existing daemon is active
        if let Some(rec) = self.read_pid_record() {
            if Self::is_pid_alive(rec.pid) {
                println!("\x1b[33m!\x1b[0m 9Router is already running (PID: {}, Port: {}).", rec.pid, rec.port);
                println!("  Run '9r status' to inspect or '9r restart' to restart.");
                return Ok(());
            } else {
                self.remove_pid_file();
            }
        }

        // Check if target port is already open and responding
        let test_addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
        if std::net::TcpStream::connect_timeout(&test_addr, Duration::from_millis(150)).is_ok() {
            println!("\x1b[33m!\x1b[0m 9Router (or another service) is already running on port {}.", port);
            println!("  Run '9r status' to inspect or '9r models' to query the active gateway.");
            return Ok(());
        }

        if !daemon {
            // Run in foreground
            println!("\x1b[36m>\x1b[0m Starting 9Router in foreground on {}:{}...", host_str, port);
            // Returning Ok(()) signals main to proceed with Axum server
            return Ok(());
        }

        fs::create_dir_all(&self.run_dir)?;
        let current_exe = std::env::current_exe()?;

        let log_out = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)?;
        let log_err = log_out.try_clone()?;

        println!("\x1b[36m>\x1b[0m Spawning 9Router background daemon on {}:{}...", host_str, port);

        let mut cmd = Command::new(&current_exe);
        cmd.arg("run");
        cmd.env("PORT", port.to_string());
        cmd.env("HOST", &host_str);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::from(log_out));
        cmd.stderr(Stdio::from(log_err));

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let child = cmd.spawn()?;
        let child_pid = child.id();

        let record = PidRecord {
            pid: child_pid,
            port,
            started_at: chrono::Utc::now().to_rfc3339(),
            exe: current_exe.to_string_lossy().to_string(),
        };
        self.write_pid_record(&record)?;

        // Wait up to 3 seconds for port to open
        let start_time = Instant::now();
        let target_addr = format!("127.0.0.1:{}", port);
        let mut ready = false;

        while start_time.elapsed() < Duration::from_millis(3000) {
            if !Self::is_pid_alive(child_pid) {
                break;
            }
            if TcpStream::connect_timeout(&target_addr.parse()?, Duration::from_millis(100)).is_ok() {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        if ready {
            println!("\x1b[32m\x1b[1m✓\x1b[0m 9Router daemon started successfully!");
            println!("  PID:      {}", child_pid);
            println!("  Endpoint: http://127.0.0.1:{}", port);
            println!("  Logs:     {}", self.log_file.display());
            println!("  Commands: '9r status', '9r logs', '9r stop'");
        } else if Self::is_pid_alive(child_pid) {
            println!("\x1b[32m\x1b[1m✓\x1b[0m 9Router daemon process spawned (PID: {}). Initializing...", child_pid);
            println!("  Logs:     {}", self.log_file.display());
        } else {
            eprintln!("\x1b[31m\x1b[1mx\x1b[0m 9Router daemon failed to start. Check logs at: {}", self.log_file.display());
            self.remove_pid_file();
            std::process::exit(1);
        }

        Ok(())
    }

    pub fn status(&self) -> anyhow::Result<()> {
        let rec = match self.read_pid_record() {
            Some(r) => r,
            None => {
                println!("\x1b[33m•\x1b[0m 9Router is stopped (no PID record found at {}).", self.pid_file.display());
                return Ok(());
            }
        };

        if !Self::is_pid_alive(rec.pid) {
            println!("\x1b[33m•\x1b[0m 9Router is stopped (stale PID {} found; process not running).", rec.pid);
            self.remove_pid_file();
            return Ok(());
        }

        // Measure RSS memory if on Linux
        let mut mem_str = String::from("N/A");
        #[cfg(target_os = "linux")]
        {
            if let Ok(statm) = fs::read_to_string(format!("/proc/{}/statm", rec.pid)) {
                let parts: Vec<&str> = statm.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Ok(resident_pages) = parts[1].parse::<u64>() {
                        let bytes = resident_pages * 4096;
                        let mb = (bytes as f64) / (1024.0 * 1024.0);
                        mem_str = format!("{:.1} MB RSS", mb);
                    }
                }
            }
        }

        let target_addr = format!("127.0.0.1:{}", rec.port);
        let socket_open = target_addr.parse::<std::net::SocketAddr>()
            .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok())
            .unwrap_or(false);

        println!("\x1b[32m\x1b[1m✓\x1b[0m 9Router is running");
        println!("   PID:        {}", rec.pid);
        println!("   Port:       {}", rec.port);
        println!("   Endpoint:   http://127.0.0.1:{}", rec.port);
        println!("   Memory:     {}", mem_str);
        println!("   Socket:     {}", if socket_open { "\x1b[32mActive (Listening)\x1b[0m" } else { "\x1b[33mPending / Closed\x1b[0m" });
        println!("   Started At: {}", rec.started_at);
        println!("   Log File:   {}", self.log_file.display());
        Ok(())
    }

    pub fn stop(&self) -> anyhow::Result<()> {
        let rec = match self.read_pid_record() {
            Some(r) => r,
            None => {
                println!("\x1b[33m•\x1b[0m 9Router is not running (no PID record found).");
                return Ok(());
            }
        };

        if !Self::is_pid_alive(rec.pid) {
            println!("\x1b[33m•\x1b[0m Process {} is already stopped.", rec.pid);
            self.remove_pid_file();
            return Ok(());
        }

        println!("\x1b[36m>\x1b[0m Stopping 9Router daemon (PID: {})...", rec.pid);

        #[cfg(unix)]
        {
            let _ = Command::new("kill").arg("-15").arg(rec.pid.to_string()).status();
        }
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill").args(["/PID", &rec.pid.to_string(), "/F"]).status();
        }

        // Wait up to 5 seconds for termination
        let start = Instant::now();
        let mut stopped = false;
        while start.elapsed() < Duration::from_millis(5000) {
            if !Self::is_pid_alive(rec.pid) {
                stopped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        if !stopped {
            #[cfg(unix)]
            {
                println!("\x1b[33m!\x1b[0m Process did not exit cleanly; sending SIGKILL...");
                let _ = Command::new("kill").arg("-9").arg(rec.pid.to_string()).status();
            }
        }

        self.remove_pid_file();
        println!("\x1b[32m\x1b[1m✓\x1b[0m 9Router daemon stopped.");
        Ok(())
    }

    pub fn restart(&self, daemon: bool, host: Option<String>, port: Option<u16>) -> anyhow::Result<()> {
        println!("\x1b[36m>\x1b[0m Restarting 9Router...");
        let _ = self.stop();
        std::thread::sleep(Duration::from_millis(200));
        self.start(daemon, host, port)
    }

    pub fn logs(&self, lines_count: usize, follow: bool) -> anyhow::Result<()> {
        if !self.log_file.exists() {
            println!("\x1b[33m•\x1b[0m No log file found at {}", self.log_file.display());
            return Ok(());
        }

        let file = File::open(&self.log_file)?;
        let reader = BufReader::new(file);
        let all_lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();

        let start_idx = if all_lines.len() > lines_count {
            all_lines.len() - lines_count
        } else {
            0
        };

        for line in &all_lines[start_idx..] {
            println!("{}", line);
        }

        if follow {
            println!("\x1b[2m--- Following {} (Ctrl+C to stop) ---\x1b[0m", self.log_file.display());
            let mut file = File::open(&self.log_file)?;
            file.seek(SeekFrom::End(0))?;
            let mut buffer = String::new();

            loop {
                buffer.clear();
                let bytes_read = (&mut file).take(65536).read_to_string(&mut buffer)?;
                if bytes_read > 0 {
                    print!("{}", buffer);
                } else {
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }

        Ok(())
    }
}
