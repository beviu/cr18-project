use std::io::{self, Read};

use clap::Parser;
use mio::{net::TcpListener, Events, Interest, Poll, Token};

#[derive(clap::Parser)]
struct Args {
    #[clap(short, long)]
    bind: String,
}

fn main() {
    let args = Args::parse();

    let mut socket = TcpListener::bind(args.bind.parse().unwrap()).unwrap();

    let mut poll = Poll::new().unwrap();
    let mut events = Events::with_capacity(128);

    const SERVER: Token = Token(usize::MAX);
    poll.registry()
        .register(&mut socket, SERVER, Interest::READABLE).unwrap();

    let mut clients = Vec::new();

    loop {
        poll.poll(&mut events, None).unwrap();

        for event in events.iter() {
            if event.token() == SERVER {
                loop {
                    let (mut client, _addr) = match socket.accept() {
                        Ok(ret) => ret,
                        Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                        Err(err) => panic!("failed to accept: {err}"),
                    };

                    let token = Token(clients.len());
                    poll.registry()
                        .register(&mut client, token, Interest::READABLE).unwrap();
                
                    clients.push(Some(client));
                }
            } else {
                let remove = if let Some(client) = &mut clients[event.token().0] {
                    loop {
                        let mut buf = [0; 256];
                        let n = match client.read(&mut buf) {
                            Ok(ret) => ret,
                            Err(err) if err.kind() == io::ErrorKind::WouldBlock => break false,
                            Err(err) => panic!("failed to read: {err}"),
                        };
                        if n == 0 {
                            break true;
                        }
                    }
                } else {
                    false
                };
                if remove {
                    clients[event.token().0] = None;
                }
            }
        }
    }
}
