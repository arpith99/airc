use tokio::net::TcpStream;
use tokio::io::{AsyncWriteExt, AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::{self, Sender, Receiver};
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let server = "irc.undernet.org";
    let channel = "#bookz";
    let username = "somedude42";
    let nickname = "somedude42";
    let realname = "Some Dude";

    let stream = TcpStream::connect(format!("{}:6667", server)).await?;

    let (sender, mut receiver): (Sender<String>, Receiver<String>) = mpsc::channel(100);
    let init_sender = sender.clone();
    let pong_sender = sender.clone();

    let (mut reader, mut writer) = stream.into_split();

    let init_task = tokio::spawn(async move {
        init_sender.send(format!("CAP LS\r\n")).await?;
        init_sender.send(format!("NICK {}\r\n", nickname)).await?;
        init_sender.send(format!("USER {} {} {} :{}\r\n", username, nickname, server, realname)).await?;
        init_sender.send(format!("JOIN {}\r\n", channel)).await?;
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    let write_task = tokio::spawn(async move {
        while let Some(receive) = receiver.recv().await {
            writer.write_all(format!("{}\r\n", receive).as_bytes()).await?;
        }
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    let read_task = tokio::spawn(async move {
        loop {
            let mut reader = BufReader::new(&mut reader);
            let mut line = String::new();

            let bytes_read = reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                break;
            }

            println!("RECV: {}", line);

            if line.starts_with("PING") {
                let pong = line.replace("PING", "PONG");
                pong_sender.send(pong).await?;
            }
        }
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    let _ = tokio::join!(init_task, write_task, read_task);

    Ok(())
}
