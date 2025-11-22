use async_std::io::stdin;
use chrono::Local;
use clap::Parser;
use colored::Colorize;
use rand::Rng;
use regex::Regex;
use std::error::Error;
use std::fs;
use std::io::{Write, copy, stdout};
use std::path::PathBuf;
use std::process::exit;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc::{self, Receiver, Sender};
use zip::ZipArchive;

const DOWNLOAD_PATH: &str = "/home/arpith/Downloads/Books/";

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

#[derive(Parser, Debug)]
struct Args {
    /// The IRC server to connect to
    #[clap(short, long)]
    server: Option<String>,
    /// The IRC channel to join
    #[clap(short, long)]
    channel: Option<String>,
    /// The username to use
    #[clap(short, long)]
    username: Option<String>,
}

impl IrcClient {
    async fn new(
        server: &str,
        channel: &str,
        username: &str,
        nickname: &str,
        realname: &str,
    ) -> Result<(Arc<IrcClient>, Receiver<String>), Box<dyn Error + Send + Sync>> {
        let stream = TcpStream::connect(format!("{}:6667", server)).await?;
        let (sender, receiver) = mpsc::channel(100);
        let (reader, writer) = stream.into_split();
        let reader = BufReader::new(reader);

        Ok((
            Arc::new(IrcClient {
                server: server.to_string(),
                channel: channel.to_string(),
                username: username.to_string(),
                nickname: nickname.to_string(),
                realname: realname.to_string(),
                reader: Arc::new(tokio::sync::Mutex::new(reader)),
                writer: Arc::new(tokio::sync::Mutex::new(writer)),
                sender,
            }),
            receiver,
        ))
    }
}

async fn init(client: Arc<IrcClient>) -> Result<(), Box<dyn Error + Send + Sync>> {
    client.sender.send(format!("CAP LS\r\n")).await?;
    client
        .sender
        .send(format!("NICK {}\r\n", client.nickname))
        .await?;
    client
        .sender
        .send(format!(
            "USER {} {} {} :{}\r\n",
            client.username, client.nickname, client.server, client.realname
        ))
        .await?;
    Ok(())
}

async fn write(
    client: Arc<IrcClient>,
    mut receiver: Receiver<String>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    while let Some(message) = receiver.recv().await {
        let mut writer = client.writer.lock().await;
        print_sent_line(&message);
        writer.write_all(format!("{}", message).as_bytes()).await?;
        if message.starts_with("QUIT") {
            print_line("Exiting...\n", true);
            print_line("Goodbye!\n", true);
            exit(0);
        }
    }
    Ok(())
}

fn process_command(client: Arc<IrcClient>, command: &str) -> String {
    let re = Regex::new(r"/s(earch)? (?P<search_term>.*)").unwrap();
    if let Some(caps) = re.captures(command) {
        let search_term = caps.name("search_term").unwrap().as_str();
        print_line(&format!("Searching for: {}", search_term), true);
        return format!("PRIVMSG {} :@search {}\r\n", client.channel, search_term);
    } else {
        // TODO Extract string from stored search result line
    }
    let message = format!("{}\r\n", command[1..].trim()); /* Strip the leading '/' */
    return message;
}

async fn cli(client: Arc<IrcClient>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut command = String::new();
    while 0 != stdin().read_line(&mut command).await? {
        match command.trim() {
            "/join" | "/j" => {
                let message = format!("JOIN {}\r\n", client.channel);
                client.sender.send(message).await?;
            }
            "/quit" | "/q" => {
                let message = format!("QUIT\r\n");
                client.sender.send(message).await?;
            }
            _ => {
                let message = process_command(client.clone(), &command);
                client.sender.send(message).await?;
            }
        }
        command.clear();
    }
    Ok(())
}

fn print_timestamp() {
    let local = Local::now();
    print!(
        "{}",
        format!("{}", local.format("[%H:%M:%S] ")).bright_black()
    );
}

fn print_sent_line(line: &str) {
    print_timestamp();
    print!("{}", format!("{}: ", "TX").red());
    print_line(line, false);
}

fn print_received_line(line: &str) {
    print_timestamp();
    print!("{}", format!("{}: ", "RX").green());
    print_line(line, false);
}

fn print_line(line: &str, ts_flag: bool) {
    let colon_index = line.find(" :").unwrap_or(0);
    let prefix = &line[..colon_index];
    let message = &line[colon_index..];
    if ts_flag {
        print_timestamp();
    }
    print!("{}", format!("{}", prefix).yellow());
    print!("{}", format!("{}", message).white());
    stdout().flush().unwrap();
}

async fn unzip_file(filename: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
    let file = fs::File::open(filename).unwrap();
    let mut archive = ZipArchive::new(file).unwrap();
    let mut file = archive.by_index(0).unwrap();
    let outpath = PathBuf::from(filename.strip_suffix(".zip").unwrap());
    let mut outfile = fs::File::create(&outpath).unwrap();
    print_line(
        &format!(
            "File {} extracted to \"{}\" ({} bytes)",
            filename,
            outpath.display(),
            file.size(),
        ),
        true,
    );
    copy(&mut file, &mut outfile).unwrap();
    Ok(outpath.to_str().unwrap().to_string())
}

async fn dcc_receive(
    filename: &str,
    ip: &str,
    port: &str,
    size: &str,
) -> Result<String, Box<dyn Error + Send + Sync>> {
    let mut stream = TcpStream::connect(format!("{}:{}", ip, port)).await?;
    let size: u32 = size[..size.len() - 2].to_string().trim().parse().unwrap(); // Strip the trailing '0x01' character and newline
    let mut buffer = vec![0; size as usize];
    stream.read_exact(&mut buffer).await?;
    let fpath = format!("{}{}", DOWNLOAD_PATH, filename);
    fs::write(&fpath, &buffer)?;
    print_line(&format!("Received file: {:?}", filename), true);
    Ok(fpath)
}

async fn process_dcc_send(line: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
    let mut fpath: String = String::new();
    let re =
        Regex::new(r".*DCC SEND (?P<filename>.*) (?P<ip>.*) (?P<port>.*) (?P<size>.*)").unwrap();
    if let Some(caps) = re.captures(&line) {
        let filename = caps.name("filename").unwrap().as_str();
        let ip = caps.name("ip").unwrap().as_str();
        let port = caps.name("port").unwrap().as_str();
        let size = caps.name("size").unwrap().as_str();
        print_line(
            &format!("Received DCC SEND request for file: {}", filename),
            true,
        );
        print_line(&format!("IP: {}, Port: {}, Size: {}", ip, port, size), true);
        fpath = dcc_receive(filename, ip, port, size).await?;
    }
    Ok(fpath)
}

async fn read_lines_to_vec(path: &str) -> Result<Vec<String>, Box<dyn Error + Send + Sync>> {
    let file = File::open(path).await?;
    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut line_stream = reader.lines();
    while let Some(line) = line_stream.next_line().await? {
        lines.push(line);
    }
    Ok(lines)
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

        print_received_line(&line);
        if line.starts_with("PING") {
            let pong = line.replace("PING", "PONG");
            client.sender.send(pong).await?;
        }
        if line.contains("DCC SEND") {
            let fpath = process_dcc_send(&line).await?;
            let path = PathBuf::from(&fpath);

            if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
                if filename.starts_with("SearchBot_results") && filename.ends_with(".zip") {
                    let txt_file = unzip_file(&fpath).await?;
                    let lines_txt_file = read_lines_to_vec(&txt_file).await?;
                    // TODO Store in a global variable
                    for (i, book_line) in lines_txt_file.into_iter().enumerate() {
                        print_line(&format!("{}: {}", i, book_line), true);
                    }
                } else {
                    // TODO Download other files like ebooks
                    print_line(&format!("Downloaded file: {}", filename), true);
                }
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let server: String;
    let channel: String;
    let username: String;
    let nickname: String;
    let realname: String;
    let args = Args::parse();

    if args.server.is_none() {
        server = "irc.undernet.org".to_string();
    } else {
        server = args.server.clone().unwrap();
    }
    if args.channel.is_none() {
        channel = "#bookz".to_string();
    } else {
        channel = args.channel.clone().unwrap();
    }
    if args.username.is_none() {
        let mut rng = rand::rng();
        let mut random_nickname: String = String::from("bworm");
        random_nickname.push_str(rng.random_range(0..=99999).to_string().as_str());
        username = random_nickname.clone();
        nickname = random_nickname.clone();
        realname = "Book Worm".to_string();
    } else {
        username = args.username.clone().unwrap();
        nickname = args.username.clone().unwrap();
        realname = args.username.clone().unwrap();
    }

    let (client, receiver) =
        IrcClient::new(&server, &channel, &username, &nickname, &realname).await?;

    let init_task = tokio::spawn(init(client.clone()));
    let write_task = tokio::spawn(write(client.clone(), receiver));
    let cli_task = tokio::spawn(cli(client.clone()));
    let read_task = tokio::spawn(read(client.clone()));

    let (init_result, write_result, cli_result, read_result) =
        tokio::join!(init_task, write_task, cli_task, read_task);

    // Propagate any errors from the tasks
    init_result??;
    write_result??;
    cli_result??;
    read_result??;

    Ok(())
}
