use std::time::Instant;

use anyhow::ensure;
use clap::Parser;
use noise_core::{GroupMembership, GroupState, Identity, Profile, SignedEvent};

#[derive(Debug, Parser)]
#[command(
    name = "noise-sim",
    about = "Generate and reduce a real Noise membership log"
)]
struct Args {
    #[arg(long, default_value_t = 50_000)]
    members: usize,
    #[arg(long, default_value_t = 3)]
    relay_copies: usize,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    ensure!(args.members > 0, "members must be greater than zero");
    ensure!(
        args.relay_copies > 0,
        "relay copies must be greater than zero"
    );

    let group = GroupMembership::create("simulation");
    let mut events = Vec::with_capacity(args.members + 1);
    let mut membership_bytes = 0usize;
    let first_identity = Identity::generate();

    let generation_started = Instant::now();
    for index in 0..args.members {
        let identity = if index == 0 {
            first_identity.clone()
        } else {
            Identity::generate()
        };
        let event = SignedEvent::member_joined(
            &identity,
            &group,
            &Profile {
                username: format!("member-{index:05}"),
                bio: String::new(),
                avatar: None,
            },
            0,
        )?;
        membership_bytes += serde_json::to_vec(&event)?.len();
        events.push(event);
    }
    let generation_elapsed = generation_started.elapsed();

    let chat = SignedEvent::chat(&first_identity, &group, "hello from member zero", 1)?;
    let chat_bytes = serde_json::to_vec(&chat)?.len();
    events.push(chat);

    let reduction_started = Instant::now();
    let state = GroupState::rebuild(&group, &events);
    let reduction_elapsed = reduction_started.elapsed();

    ensure!(
        state.members.len() == args.members,
        "expected {} active members, reconstructed {}",
        args.members,
        state.members.len()
    );
    ensure!(
        state.messages.len() == 1,
        "the post-join message was not accepted"
    );
    ensure!(
        state.rejected_events == 0,
        "reducer rejected {} events",
        state.rejected_events
    );

    let generation_rate = args.members as f64 / generation_elapsed.as_secs_f64();
    let reduction_rate = events.len() as f64 / reduction_elapsed.as_secs_f64();
    let replicated_membership_bytes = membership_bytes * args.relay_copies;
    let replicated_chat_bytes = chat_bytes * args.relay_copies;
    let per_recipient_chat_bytes = chat_bytes * args.members;

    println!("noise 50k protocol simulation");
    println!("members reconstructed       {}", state.members.len());
    println!("signed join events          {}", args.members);
    println!("rejected events             {}", state.rejected_events);
    println!(
        "membership log              {}",
        human_bytes(membership_bytes)
    );
    println!(
        "membership across {} relays  {}",
        args.relay_copies,
        human_bytes(replicated_membership_bytes)
    );
    println!(
        "average join event          {} bytes",
        membership_bytes / args.members
    );
    println!(
        "join generation             {:.3}s ({:.0} events/s)",
        generation_elapsed.as_secs_f64(),
        generation_rate
    );
    println!(
        "verify + decrypt + reduce    {:.3}s ({:.0} events/s)",
        reduction_elapsed.as_secs_f64(),
        reduction_rate
    );
    println!("one encrypted message       {} bytes", chat_bytes);
    println!(
        "stored once on {} relays     {}",
        args.relay_copies,
        human_bytes(replicated_chat_bytes)
    );
    println!(
        "naive per-member fanout      {}",
        human_bytes(per_recipient_chat_bytes)
    );
    if cfg!(debug_assertions) {
        println!("note: run with --release for meaningful timing");
    }
    Ok(())
}

fn human_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let bytes = bytes as f64;
    if bytes >= MIB {
        format!("{:.2} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}
