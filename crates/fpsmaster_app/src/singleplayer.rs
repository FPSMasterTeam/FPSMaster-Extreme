use std::io::Write;
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub struct LocalServer {
    child: Child,
    port: u16,
}

const SERVER_DIR: &str = "local_server/paper-1.8-protocol47";
const SERVER_JAR: &str = "paper-1.8.8-445.jar";
const SERVER_PORT: u16 = 25565;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

impl LocalServer {
    pub fn start(username: &str) -> Result<(Self, Receiver<bool>), String> {
        let server_dir = std::path::PathBuf::from(SERVER_DIR);
        let jar = server_dir.join(SERVER_JAR);

        if !jar.exists() {
            return Err(format!("Server jar not found: {}", jar.display()));
        }

        let mut child = Command::new("java")
            .args(["-Xms512M", "-Xmx1G", "-jar"])
            .arg(SERVER_JAR)
            .arg("nogui")
            .current_dir(&server_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start server: {e}"))?;

        let stdin = child.stdin.take();
        let (tx, rx) = mpsc::channel();
        let port = SERVER_PORT;
        let op_name = username.to_owned();
        std::thread::spawn(move || {
            let deadline = Instant::now() + STARTUP_TIMEOUT;
            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
            loop {
                if Instant::now() > deadline {
                    let _ = tx.send(false);
                    return;
                }
                if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
                    if let Some(mut stdin) = stdin {
                        let _ = writeln!(stdin, "op {op_name}");
                    }
                    let _ = tx.send(true);
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });

        Ok((LocalServer { child, port }, rx))
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.kill();
    }
}
