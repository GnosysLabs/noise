use std::path::PathBuf;

use clap::{Parser, Subcommand};
use noise_client::NoiseClient;

#[derive(Debug, Parser)]
#[command(name = "noise", about = "The Noise protocol laboratory client")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate a local identity with no phone number or email address.
    Init {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        username: String,
    },
    /// Create a group and publish its frequency invitation.
    Make {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long)]
        relay: Vec<String>,
    },
    /// Join a group by entering its frequency.
    Join {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        frequency: String,
        #[arg(long)]
        relay: Vec<String>,
    },
    /// Send a message to the active group.
    Say {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        text: String,
        #[arg(long)]
        relay: Vec<String>,
    },
    /// Merge and display the active group's history from available relays.
    Read {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        relay: Vec<String>,
    },
    /// Wait privately until the active group's revision changes.
    Watch {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        since: Option<u64>,
        #[arg(long)]
        relay: Vec<String>,
    },
    /// Display membership reconstructed from signed group events.
    Members {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        relay: Vec<String>,
    },
    /// Update the active group's founder-controlled identity.
    GroupProfile {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, default_value = "")]
        rules: String,
        #[arg(long)]
        relay: Vec<String>,
    },
    /// Publish a signed departure from the active group.
    Leave {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        relay: Vec<String>,
    },
    /// Permanently delete a group you founded.
    Delete {
        #[arg(long)]
        state: PathBuf,
        #[arg(long)]
        relay: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let client = NoiseClient::default();

    match args.command {
        Command::Init { state, username } => {
            let summary = client.initialize(state, username)?;
            println!("identity ready: @{}", summary.identity.username);
            println!("public key: {}", summary.identity.public_key);
        }
        Command::Make { state, name, relay } => {
            let result = client.make(state, name, None, None, relay).await?;
            println!("you made noise: {}", result.group.name);
            println!("frequency");
            println!("{}", result.display_frequency);
        }
        Command::Join {
            state,
            frequency,
            relay,
        } => {
            let result = client.join(state, &frequency, relay).await?;
            println!("joined {}", result.group.name);
        }
        Command::Say { state, text, relay } => {
            client.say(state, text, relay).await?;
            println!("sent");
        }
        Command::Read { state, relay } => {
            let conversation = client.conversation(state, relay).await?;
            println!(
                "{} · {}",
                conversation.group.name,
                member_count(conversation.members.len())
            );
            if conversation.messages.is_empty() {
                println!("no messages yet");
            }
            for message in conversation.messages {
                println!("@{}  {}", message.username, message.text);
            }
            if conversation.rejected_events > 0 {
                eprintln!(
                    "ignored {} invalid group event(s)",
                    conversation.rejected_events
                );
            }
        }
        Command::Watch {
            state,
            since,
            relay,
        } => {
            let change = client.watch_group(state, since, relay).await?;
            println!("{} {}", change.revision, change.changed);
        }
        Command::Members { state, relay } => {
            let conversation = client.conversation(state, relay).await?;
            println!("{}", member_count(conversation.members.len()));
            for member in conversation.members {
                println!("@{}  {}", member.username, member.public_key);
            }
            if conversation.rejected_events > 0 {
                eprintln!(
                    "ignored {} invalid group event(s)",
                    conversation.rejected_events
                );
            }
        }
        Command::GroupProfile {
            state,
            name,
            description,
            rules,
            relay,
        } => {
            client
                .update_group_profile(
                    &state,
                    name,
                    description,
                    rules,
                    None,
                    None,
                    false,
                    relay.clone(),
                )
                .await?;
            let conversation = client.conversation(state, relay).await?;
            println!(
                "updated {} · {}",
                conversation.group.name, conversation.group.description
            );
        }
        Command::Leave { state, relay } => {
            client.leave(state, relay).await?;
            println!("left active group");
        }
        Command::Delete { state, relay } => {
            let group_id = client
                .local_summary(&state)?
                .groups
                .into_iter()
                .find(|group| group.is_active)
                .map(|group| group.group_id)
                .ok_or_else(|| anyhow::anyhow!("no active group"))?;
            client.delete_group(state, &group_id, relay).await?;
            println!("deleted group");
        }
    }

    Ok(())
}

fn member_count(count: usize) -> String {
    match count {
        1 => "1 member".to_owned(),
        count => format!("{count} members"),
    }
}
