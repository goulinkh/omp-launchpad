mod diff;
mod error;
mod launchpad;
mod local_git;
mod render;
mod request;
mod response;
mod result;

use std::env;
use std::io::{self, Read};

use lpcli::{auth, status};

use crate::error::Error;
use crate::request::Request;
use crate::response::BridgeResponse;
use crate::result::Result;

#[tokio::main]
async fn main() {
    let arguments: Vec<_> = env::args().skip(1).collect();
    if !arguments.is_empty() {
        if let Err(error) = run_command(&arguments).await {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    let response = run_bridge().await;
    if let Err(source) = serde_json::to_writer(io::stdout(), &response) {
        eprintln!("cannot serialise bridge response: {source}");
        std::process::exit(1);
    }
    println!();
}

async fn run_command(arguments: &[String]) -> Result<()> {
    match arguments {
        [command] if command == "login" => {
            auth::login().await?;
            println!("Logged in to Launchpad.");
        }
        [command] if command == "logout" => {
            auth::logout()?;
        }
        [command] if command == "status" => {
            print_status().await;
        }
        _ => {
            return Err(Error::invalid(
                "expected one command: login, logout, or status",
            ));
        }
    }
    Ok(())
}

async fn print_status() {
    let (server, authentication) = tokio::join!(status::check_server(), status::check_auth());
    if server.reachable {
        println!("Launchpad API: reachable");
    } else if let Some(error) = server.error {
        println!("Launchpad API: unreachable ({error})");
    } else {
        println!("Launchpad API: unreachable");
    }

    match (authentication.logged_in, authentication.username) {
        (true, Some(username)) => println!("Authentication: logged in as {username}"),
        (true, None) => println!("Authentication: credentials found but not verified"),
        (false, _) => println!("Authentication: not logged in"),
    }
}

async fn run_bridge() -> BridgeResponse {
    let mut request = String::new();
    if let Err(source) = io::stdin().read_to_string(&mut request) {
        return BridgeResponse::failure(format!("cannot read bridge request: {source}"));
    }
    let request: Request = match serde_json::from_str(&request) {
        Ok(request) => request,
        Err(source) => {
            return BridgeResponse::failure(format!("cannot parse bridge request: {source}"));
        }
    };
    match launchpad::execute(&request).await {
        Ok(result) => BridgeResponse::success(result),
        Err(error) => BridgeResponse::failure_error(&error),
    }
}
