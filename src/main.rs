use std::collections::BinaryHeap;
use std::collections::HashMap;
use std::fmt::Display;

use chrono::{DateTime, Duration, FixedOffset, Utc};
use futures::stream::FuturesUnordered;
use futures::stream::StreamExt;
use octocrab::{Octocrab, OctocrabBuilder};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PRKind {
    Feature,
    BugFix,
    Internal,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Commit {
    author: String,
    message: String,
    pr_number: String,
    date: DateTime<FixedOffset>,
}

impl Display for Commit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let host = "https://github.com";
        let owner = "boa-dev";
        let repo = "boa";
        write!(
            f,
            "{} by @{} in [#{}]({}/{}/{}/pull/{})",
            &self.message, &self.author, &self.pr_number, host, owner, repo, &self.pr_number
        )
    }
}

impl Ord for Commit {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.date.cmp(&other.date)
    }
}

impl PartialOrd for Commit {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl TryFrom<&Value> for Commit {
    type Error = ();

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        fn get_pr_number(message: &str) -> Result<String, ()> {
            // TODO: this can probably be improved
            let right_of_parens = message.split_once(" (#").ok_or(())?.1;
            let pr_number_str = right_of_parens.split_once(')').ok_or(())?.0.to_string();
            debug_assert!(pr_number_str.chars().all(|c| c.is_digit(10)));
            Ok(pr_number_str)
        }

        let node = value.get("node").ok_or(())?;
        let author = node
            .get("author")
            .and_then(|map| map.get("user"))
            .and_then(|map| map.get("login"))
            .and_then(Value::as_str)
            .ok_or(())?
            .to_string();
        // we get just the first line
        let first_line = node
            .get("message")
            .and_then(Value::as_str)
            .ok_or(())?
            .lines()
            .next()
            .expect("commit message can't be empty")
            .to_string();
        let pr_number = get_pr_number(&first_line)?;
        let message = {
            let idx = first_line.find(" (#").ok_or(())?;
            first_line[..idx].trim().to_string()
        };
        let date: DateTime<_> = node
            .get("authoredDate")
            .and_then(Value::as_str)
            .map(chrono::DateTime::parse_from_rfc3339)
            .and_then(Result::ok)
            .ok_or(())?;
        Ok(Commit {
            author,
            message,
            pr_number,
            date,
        })
    }
}

async fn get_commits(date_last_release: &str, crab: &Octocrab) -> Result<Vec<Commit>, ()> {
    async fn get_100_commits(
        date_last_release: &str,
        until_date: DateTime<FixedOffset>,
        crab: &Octocrab,
    ) -> Result<(Vec<Commit>, DateTime<FixedOffset>), ()> {
        let query = String::from(
            r#"
    query {
      repository(owner:"boa-dev", name:"boa") {
        refs(refPrefix:"refs/heads/", query:"main", last:1) {
          edges {
            node {
              target {
                ... on Commit {
                  history(since:"$date_last_release", until: "$until_date") {
                    edges {
                      node {
                        author {
                          user {
                            login
                          }
                        }
                        message
                        authoredDate
                      }
                    }
                  }
                }
              }
            }
          }
        }
      }
    }
            "#,
        )
        .replace("$date_last_release", date_last_release)
        .replace("$until_date", &until_date.to_rfc3339());
        let response_object: serde_json::Value = crab
            .graphql(&query)
            .await
            .expect("TODO, should handle this");
        let vec = response_object
            .get("data")
            .and_then(|obj| obj.get("repository"))
            .and_then(|obj| obj.get("refs"))
            .and_then(|obj| obj.get("edges"))
            .and_then(|obj| obj.get(0))
            .and_then(|obj| obj.get("node"))
            .and_then(|obj| obj.get("target"))
            .and_then(|obj| obj.get("history"))
            .and_then(|obj| obj.get("edges"))
            .and_then(Value::as_array)
            .cloned()
            .ok_or(())?;
        let new_date: DateTime<_> = vec
            .last()
            .and_then(|obj| obj.get("node"))
            .and_then(|obj| obj.get("authoredDate"))
            .and_then(Value::as_str)
            .map(chrono::DateTime::parse_from_rfc3339)
            .and_then(Result::ok)
            .ok_or(())?
            .checked_sub_signed(Duration::seconds(1))
            .expect("can't undeflow by subtracting 1 second from a commit's date");
        let res = vec
            .iter()
            // We ignore commits if we can't find the PR number
            .flat_map(|obj| Commit::try_from(obj).ok())
            .collect();
        Ok((res, new_date))
    }
    // each query only gets 100 commits, so we need to do it in batches of 100
    // we use the date of the last one we received to paginate
    let now = Utc::now();
    let now = now.with_timezone(&FixedOffset::east(0));

    let (mut ret, mut until_date) = get_100_commits(date_last_release, now, crab).await?;
    loop {
        let res = get_100_commits(date_last_release, until_date, crab).await;
        match res {
            Ok((new_100, new_until)) => {
                if new_100.is_empty() {
                    break;
                }
                ret.extend(new_100);
                until_date = new_until;
            }
            Err(_) => break,
        }
    }
    Ok(ret)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PR {
    commit: Commit,
    kind: PRKind,
}

impl Ord for PR {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.commit.cmp(&other.commit)
    }
}

impl PartialOrd for PR {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.commit.partial_cmp(&other.commit)
    }
}

async fn pr_from_commit(commit: Commit, crab: &Octocrab) -> Result<PR, ()> {
    // TODO: get mapping from config
    let mapping: HashMap<&str, PRKind> = HashMap::from([
        ("enhancement", PRKind::Feature),
        ("bug", PRKind::BugFix),
        ("Internal", PRKind::Internal),
    ]);
    let query = String::from(
        r#"
query {
  repository(owner:"boa-dev", name:"boa") {
    pullRequest(number:$pr_number) {
      labels(first: 10) {
        edges {
          node {
            name
          }
        }
      }
    }
  }
}"#,
    )
    .replace("$pr_number", &commit.pr_number);
    let response_object: serde_json::Value = crab
        .graphql(&query)
        .await
        .expect("TODO, should handle this");
    let label_array = response_object
        .get("data")
        .and_then(|obj| obj.get("repository"))
        .and_then(|obj| obj.get("pullRequest"))
        .and_then(|obj| obj.get("labels"))
        .and_then(|obj| obj.get("edges"))
        .and_then(Value::as_array)
        .ok_or(())?;
    for label in label_array
        .iter()
        .flat_map(|object| object.get("node"))
        .flat_map(|object| object.get("name"))
        .flat_map(Value::as_str)
    {
        if let Some(kind) = mapping.get(label) {
            return Ok(PR {
                commit,
                kind: *kind,
            });
        }
    }
    Ok(PR {
        commit,
        kind: PRKind::Ignored,
    })
}

#[tokio::main]
async fn main() -> Result<(), ()> {
    // TODO: take the *owner* and *repo* as arguments and use them to read the config
    // from main.
    // Somehow get the date of the latest tag, instead of receiving it (currently hard-coded)
    let token = std::env::var("GITHUB_TOKEN").expect("TODO, die gracefuly");
    let crab = OctocrabBuilder::new()
        .personal_token(token)
        .build()
        .expect("TODO, die gracefuly");

    let date_last_release = "2021-09-30T00:00:00"; // TODO: should get date of last release magically
    eprintln!("Fetching all commits since last release");
    let commits = get_commits(date_last_release, &crab).await?;
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
    println!("");
    for feat in features.into_sorted_vec().iter() {
        println!("- {}", feat.commit);
    }
    println!("");

    println!("### Bug Fixes");
    println!("");
    for fix in fixes.into_sorted_vec().iter() {
        println!("- {}", fix.commit);
    }
    println!("");

    println!("### Internal Improvements");
    println!("");
    for improvement in improvements.into_sorted_vec().iter() {
        println!("- {}", improvement.commit);
    }
    Ok(())
}
