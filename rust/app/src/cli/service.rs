use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub struct ServiceManager;

impl ServiceManager {
    pub fn install() -> anyhow::Result<()> {
        let current_exe = std::env::current_exe()?;
        let exe_str = current_exe.to_string_lossy();
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());

        #[cfg(target_os = "linux")]
        {
            let unit_dir = PathBuf::from(&home).join(".config/systemd/user");
            fs::create_dir_all(&unit_dir)?;
            let service_file = unit_dir.join("9router.service");

            let service_content = format!(
                r#"[Unit]
Description=9Router Native AI Gateway and Dashboard
After=network.target

[Service]
Type=simple
WorkingDirectory={home}
ExecStart={exe_str} run
Restart=always
RestartSec=3
Environment="PORT=20127"
Environment="RUST_LOG=info"
LimitNOFILE=65536
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=default.target
"#,
                home = home,
                exe_str = exe_str
            );

            fs::write(&service_file, service_content)?;
            println!("\x1b[32m\x1b[1m✓\x1b[0m Wrote systemd user service to: {}", service_file.display());

            let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
            let status = Command::new("systemctl").args(["--user", "enable", "--now", "9router"]).status();

            match status {
                Ok(s) if s.success() => {
                    println!("\x1b[32m\x1b[1m✓\x1b[0m Service enabled and started via: systemctl --user enable --now 9router");
                }
                _ => {
                    println!("\x1b[33m!\x1b[0m Service file created. Enable manually with:");
                    println!("    systemctl --user daemon-reload && systemctl --user enable --now 9router");
                }
            }
            return Ok(());
        }

        #[cfg(target_os = "macos")]
        {
            let agent_dir = PathBuf::from(&home).join("Library/LaunchAgents");
            fs::create_dir_all(&agent_dir)?;
            let plist_file = agent_dir.join("com.9router.gateway.plist");

            let plist_content = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.9router.gateway</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe_str}</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{home}/.9router/run/gateway.log</string>
    <key>StandardErrorPath</key>
    <string>{home}/.9router/run/gateway.log</string>
</dict>
</plist>
"#,
                exe_str = exe_str,
                home = home
            );

            fs::write(&plist_file, plist_content)?;
            println!("\x1b[32m\x1b[1m✓\x1b[0m Wrote launchd agent to: {}", plist_file.display());
            let _ = Command::new("launchctl").arg("load").arg(&plist_file).status();
            println!("\x1b[32m\x1b[1m✓\x1b[0m Loaded launchd agent com.9router.gateway");
            return Ok(());
        }

        #[cfg(target_os = "windows")]
        {
            let tr = format!("\"{}\" run", exe_str);
            let status = Command::new("schtasks")
                .args(["/Create", "/SC", "ONLOGON", "/TN", "9Router", "/TR", &tr, "/F"])
                .status()?;
            if status.success() {
                println!("\x1b[32m\x1b[1m✓\x1b[0m Created Windows Scheduled Task '9Router'");
            }
            return Ok(());
        }

        #[allow(unreachable_code)]
        Ok(())
    }

    pub fn uninstall() -> anyhow::Result<()> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());

        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("systemctl").args(["--user", "stop", "9router"]).status();
            let _ = Command::new("systemctl").args(["--user", "disable", "9router"]).status();
            let service_file = PathBuf::from(&home).join(".config/systemd/user/9router.service");
            if service_file.exists() {
                let _ = fs::remove_file(&service_file);
            }
            let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
            println!("\x1b[32m\x1b[1m✓\x1b[0m 9Router systemd user service removed.");
            return Ok(());
        }

        #[cfg(target_os = "macos")]
        {
            let plist_file = PathBuf::from(&home).join("Library/LaunchAgents/com.9router.gateway.plist");
            if plist_file.exists() {
                let _ = Command::new("launchctl").arg("unload").arg(&plist_file).status();
                let _ = fs::remove_file(&plist_file);
            }
            println!("\x1b[32m\x1b[1m✓\x1b[0m 9Router launchd agent removed.");
            return Ok(());
        }

        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("schtasks").args(["/Delete", "/TN", "9Router", "/F"]).status();
            println!("\x1b[32m\x1b[1m✓\x1b[0m 9Router scheduled task removed.");
            return Ok(());
        }

        #[allow(unreachable_code)]
        Ok(())
    }

    pub fn status() -> anyhow::Result<()> {
        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("systemctl").args(["--user", "status", "9router"]).status();
            return Ok(());
        }

        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("launchctl").args(["list", "com.9router.gateway"]).status();
            return Ok(());
        }

        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("schtasks").args(["/Query", "/TN", "9Router"]).status();
            return Ok(());
        }

        #[allow(unreachable_code)]
        Ok(())
    }
}
