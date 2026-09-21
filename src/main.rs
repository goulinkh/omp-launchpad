mod error;
mod launchpad;
mod local_git;
mod render;
mod request;
mod response;
mod result;

use std::io::{self, Read};

use crate::request::Request;
use crate::response::BridgeResponse;

#[tokio::main]
async fn main() {
    let response = run().await;
    if let Err(source) = serde_json::to_writer(io::stdout(), &response) {
        eprintln!("cannot serialise bridge response: {source}");
        std::process::exit(1);
    }
    println!();
}

async fn run() -> BridgeResponse {
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
        Err(error) => BridgeResponse::failure(error),
    }
}
