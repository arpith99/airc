use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::io::{AsyncWriteExt, AsyncBufReadExt, BufReader};
use tokio::sync::mpsc::{self, Sender, Receiver};
use std::error::Error;
use std::sync::Arc;

#[derive(Clone)]
struct IrcClient {
    server: String,
    channel: String,
    username: String,
    nickname: String,
    realname: String,
    reader: Arc<tokio::sync::Mutex<BufReader<OwnedReadHalf>>>,
    writer: Arc<tokio::sync::Mutex<OwnedWriteHalf>>,
    sender: Sender<String>,
}

impl IrcClient {
    async fn new(server: &str, channel: &str, username: &str, nickname: &str, realname: &str) 
        -> Result<(Arc<IrcClient>, Receiver<String>), Box<dyn Error + Send + Sync>> {
        let stream = TcpStream::connect(format!("{}:6667", server)).await?;
        let (sender, receiver) = mpsc::channel(100);
        let (reader, writer) = stream.into_split();
        let reader = BufReader::new(reader);
        
        Ok((Arc::new(IrcClient {
            server: server.to_string(),
            channel: channel.to_string(),
            username: username.to_string(),
            nickname: nickname.to_string(),
            realname: realname.to_string(),
            reader: Arc::new(tokio::sync::Mutex::new(reader)),
            writer: Arc::new(tokio::sync::Mutex::new(writer)),
            sender,
        }), receiver))
    }
}

async fn init(client: Arc<IrcClient>) -> Result<(), Box<dyn Error + Send + Sync>> {
    client.sender.send(format!("CAP LS\r\n")).await?;
    client.sender.send(format!("NICK {}\r\n", client.nickname)).await?;
    client.sender.send(format!("USER {} {} {} :{}\r\n", client.username, client.nickname, client.server, client.realname)).await?;
    client.sender.send(format!("JOIN {}\r\n", client.channel)).await?;
    Ok(())
}

async fn write(client: Arc<IrcClient>, mut receiver: Receiver<String>) -> Result<(), Box<dyn Error + Send + Sync>> {
    while let Some(message) = receiver.recv().await {
        let mut writer = client.writer.lock().await;
        writer.write_all(format!("{}", message).as_bytes()).await?;
    }
    Ok(())
}

async fn read(client: Arc<IrcClient>) -> Result<(), Box<dyn Error + Send + Sync>> {
    loop {
        let mut line = String::new();
        let bytes_read = {
            let mut reader = client.reader.lock().await;
            reader.read_line(&mut line).await?
        };
        
        if bytes_read == 0 {
            break;
        }
        
        println!("RECV: {}", line);
        if line.starts_with("PING") {
            let pong = line.replace("PING", "PONG");
            client.sender.send(pong).await?;
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (client, receiver) = IrcClient::new(
        "irc.undernet.org", 
        "#bookz", 
        "somedude42", 
        "somedude42", 
        "Some Dude"
    ).await?;

    let init_task = tokio::spawn(init(client.clone()));
    let write_task = tokio::spawn(write(client.clone(), receiver));
    let read_task = tokio::spawn(read(client.clone()));

    let (init_result, write_result, read_result) = tokio::join!(init_task, write_task, read_task);
    
    // Propagate any errors from the tasks
    init_result??;
    write_result??;
    read_result??;

    Ok(())
}
