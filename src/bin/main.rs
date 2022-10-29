use std::collections::BinaryHeap;

use clap::Parser;
use futures::stream::FuturesUnordered;
use futures::stream::StreamExt;
use octocrab::OctocrabBuilder;

use crab_log::{get_commits, pr_from_commit, Config, PRKind};

#[tokio::main]
async fn main() -> Result<(), ()> {
    // TODO: use *owner* and *repo* to read the config from HEAD on GH
    // config: who to ignore + labels
    let token = std::env::var("GITHUB_TOKEN").expect("Missing GITHUB_TOKEN env var");
    let config = Config::parse();
    let crab = OctocrabBuilder::new()
        .personal_token(token)
        .build()
        .expect("TODO, die gracefuly");

    let date_last_release = "2022-06-11T00:00:00"; // TODO: should get date of last release magically
    eprintln!("Fetching all commits since last release");
    let commits = get_commits(&config, date_last_release, &crab).await?;
    eprintln!("commits:      {:3}", commits.len());

    let mut futs: FuturesUnordered<_> = commits
        .into_iter()
        // TODO: make this configurable
        // disregard commits by dependabot
        .filter(|com| !com.author.contains("dependabot"))
        .map(|commit| pr_from_commit(commit, &crab))
        .collect();
    eprintln!("user commits: {:3}", futs.len());

    let mut features = BinaryHeap::new();
    let mut fixes = BinaryHeap::new();
    let mut improvements = BinaryHeap::new();
    let mut ignored = BinaryHeap::new();
    while let Some(re) = futs.next().await {
        // put PR in appropriate list
        if let Ok(pr) = re {
            match pr.kind {
                PRKind::Feature => {
                    features.push(pr);
                }
                PRKind::BugFix => {
                    fixes.push(pr);
                }
                PRKind::Internal => {
                    improvements.push(pr);
                }
                PRKind::Ignored => {
                    ignored.push(pr);
                }
            }
        }
    }
    eprintln!("features:     {:3}", features.len());
    eprintln!("fixes:        {:3}", fixes.len());
    eprintln!("improvements: {:3}", improvements.len());
    eprintln!("ignored:      {:3}", ignored.len());

    println!("### Feature Enhancements");
    println!();
    for feat in features.into_sorted_vec().iter() {
        println!("- {}", feat.commit);
    }
    println!();

    println!("### Bug Fixes");
    println!();
    for fix in fixes.into_sorted_vec().iter() {
        println!("- {}", fix.commit);
    }
    println!();

    println!("### Internal Improvements");
    println!();
    for improvement in improvements.into_sorted_vec().iter() {
        println!("- {}", improvement.commit);
    }
    Ok(())
}
