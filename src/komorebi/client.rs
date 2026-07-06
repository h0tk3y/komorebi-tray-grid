//! Client for sending commands to komorebi.

use anyhow::{Context, Result};
use komorebi_client::{send_message, SocketMessage};

pub fn send_command(msg: SocketMessage) -> Result<()> {
    send_message(&msg).context("send message to komorebi")?;
    Ok(())
}
