use webrtc_sctp::association::*;
use webrtc_sctp::error::*;
use webrtc_sctp::stream::*;

use async_trait::async_trait;
use bytes::Bytes;
use clap::{App, AppSettings, Arg};
use std::io;
//use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::signal;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use util::Conn;

// RUST_LOG=trace cargo run --color=always --package webrtc-sctp --example pong -- --host 0.0.0.0:5678

#[tokio::main]
async fn main() -> Result<(), Error> {
    /*env_logger::Builder::new()
    .format(|buf, record| {
        writeln!(
            buf,
            "{}:{} [{}] {} - {}",
            record.file().unwrap_or("unknown"),
            record.line().unwrap_or(0),
            record.level(),
            chrono::Local::now().format("%H:%M:%S.%6f"),
            record.args()
        )
    })
    .filter(None, log::LevelFilter::Trace)
    .init();*/

    let mut app = App::new("SCTP Pong")
        .version("0.1.0")
        .author("Rain Liu <yliu@webrtc.rs>")
        .about("An example of SCTP Server")
        .setting(AppSettings::DeriveDisplayOrder)
        .setting(AppSettings::SubcommandsNegateReqs)
        .arg(
            Arg::with_name("FULLHELP")
                .help("Prints more detailed help information")
                .long("fullhelp"),
        )
        .arg(
            Arg::with_name("host")
                .required_unless("FULLHELP")
                .takes_value(true)
                .long("host")
                .help("SCTP host name."),
        );

    let matches = app.clone().get_matches();

    if matches.is_present("FULLHELP") {
        app.print_long_help().unwrap();
        std::process::exit(0);
    }

    let host = matches.value_of("host").unwrap();
    let conn = DisconnectedPacketConn::new(Arc::new(UdpSocket::bind(host).await.unwrap()));
    println!("listening {}...", conn.local_addr().await.unwrap());

    let config = Config {
        net_conn: Arc::new(conn),
        max_receive_buffer_size: 0,
        max_message_size: 0,
        name: "server".to_owned(),
    };
    let mut a = Association::server(config).await?;
    println!("created a server");

    let stream = a.accept_stream().await.unwrap();
    println!("accepted a stream");

    // set unordered = true and 10ms treshold for dropping packets
    stream.set_reliability_params(true, ReliabilityType::Timed, 10);

    let (done_tx, mut done_rx) = mpsc::channel::<()>(1);
    let stream2 = Arc::clone(&stream);
    tokio::spawn(async move {
        let mut buff = vec![0u8; 1024];
        while let Ok(n) = stream2.read(&mut buff).await {
            let ping_msg = String::from_utf8(buff[..n].to_vec()).unwrap();
            println!("received: {}", ping_msg);

            let pong_msg = format!("pong [{}]", ping_msg);
            println!("sent: {}", pong_msg);
            stream2.write(&Bytes::from(pong_msg)).await?;

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        println!("finished ping-pong");
        drop(done_tx);
        Ok::<(), Error>(())
    });

    println!("Waiting for Ctrl-C...");
    signal::ctrl_c().await.expect("failed to listen for event");
    println!("Closing stream and association...");

    stream.close().await?;
    a.close().await?;

    let _ = done_rx.recv().await;

    Ok(())
}

/// Reference: https://github.com/pion/sctp/blob/master/association_test.go
/// Since UDP is connectionless, as a server, it doesn't know how to reply
/// simply using the `Write` method. So, to make it work, `disconnectedPacketConn`
/// will infer the last packet that it reads as the reply address for `Write`
struct DisconnectedPacketConn {
    raddr: Mutex<SocketAddr>,
    pconn: Arc<dyn Conn + Send + Sync>,
}

impl DisconnectedPacketConn {
    fn new(conn: Arc<dyn Conn + Send + Sync>) -> impl Conn {
        DisconnectedPacketConn {
            raddr: Mutex::new(SocketAddr::new(Ipv4Addr::new(0, 0, 0, 0).into(), 0)),
            pconn: conn,
        }
    }
}

#[async_trait]
impl Conn for DisconnectedPacketConn {
    async fn connect(&self, addr: SocketAddr) -> io::Result<()> {
        self.pconn.connect(addr).await
    }

    async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        let (n, addr) = self.pconn.recv_from(buf).await?;
        {
            let mut raddr = self.raddr.lock().await;
            *raddr = addr;
        }
        Ok(n)
    }

    async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.pconn.recv_from(buf).await
    }

    async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        let addr = {
            let raddr = self.raddr.lock().await;
            *raddr
        };
        self.pconn.send_to(buf, addr).await
    }

    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
        self.pconn.send_to(buf, target).await
    }

    async fn local_addr(&self) -> io::Result<SocketAddr> {
        self.pconn.local_addr().await
    }
}
