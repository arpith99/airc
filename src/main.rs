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

    let (mut reader, mut writer) = stream.into_split();

    let write_task = tokio::spawn(async move {
        writer.write_all(format!("CAP LS\r\n").as_bytes()).await?;
        writer.write_all(format!("NICK {}\r\n", nickname).as_bytes()).await?;
        writer.write_all(format!("USER {} {} {} :{}\r\n", username, nickname, server, realname).as_bytes()).await?;
        writer.write_all(format!("JOIN {}\r\n", channel).as_bytes()).await?;
        while let Some(pong) = receiver.recv().await {
            writer.write_all(format!("{}\r\n", pong).as_bytes()).await?;
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
                sender.send(pong).await?;
            }
        }
        Ok::<_, Box<dyn Error + Send + Sync>>(())
    });

    let _ = tokio::try_join!(write_task, read_task)?;

    Ok(())
}
