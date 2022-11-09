use std::collections::{HashMap, BinaryHeap};
use std::fmt::Display;

use chrono::{DateTime, Duration, FixedOffset, Utc};
use clap::Parser;
use octocrab::Octocrab;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit<'c> {
    pub config: &'c Config,
    pub author: String,
    pub message: String,
    pub pr_number: String,
    pub date: DateTime<FixedOffset>,
}

impl<'c> Display for Commit<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let host = "https://github.com";
        let owner = &self.config.owner;
        let repo = &self.config.repo;
        write!(
            f,
            "{} by @{} in [#{}]({}/{}/{}/pull/{})",
            &self.message, &self.author, &self.pr_number, host, owner, repo, &self.pr_number
        )
    }
}

impl Ord for Commit<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.date.cmp(&other.date)
    }
}

impl PartialOrd for Commit<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Parser, Debug, PartialEq, Eq)]
#[command(author, version, about, long_about = None)]
pub struct Config {
    #[arg(short, long)]
    owner: String,

    #[arg(short, long)]
    repo: String,
}

impl Config {
    pub fn build_commit(&self, value: &Value) -> Result<Commit, ()> {
        fn get_pr_number(message: &str) -> Result<String, ()> {
            // TODO: this can probably be improved
            let right_of_parens = message.split_once(" (#").ok_or(())?.1;
            let pr_number_str = right_of_parens.split_once(')').ok_or(())?.0.to_string();
            debug_assert!(pr_number_str.chars().all(|c| c.is_ascii_digit()));
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
            config: self,
            author,
            message,
            pr_number,
            date,
        })
    }

    pub async fn get_date_last_release(&self, crab: &Octocrab) -> Result<String, ()> {
        let owner = &self.owner;
        let repo = &self.repo;
        let query = format!(
            r#"
query {{
  repository(owner:"{owner}", name:"{repo}") {{
    latestRelease {{
      tagCommit {{
        committedDate
      }}
    }}
  }}
}}
            "#,
        );
        let response_object: Value = crab
            .graphql(&query)
            .await
            .expect("fetching last release failed");
        response_object.get("data")
            .and_then(|obj| obj.get("repository"))
            .and_then(|obj| obj.get("latestRelease"))
            .and_then(|obj| obj.get("tagCommit"))
            .and_then(|obj| obj.get("committedDate"))
            .and_then(Value::as_str)
            .map(Into::into)
            .ok_or(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PRKind {
    Feature,
    BugFix,
    Internal,
    Ignored,
}

pub async fn get_commits<'c>(
    config: &'c Config,
    date_last_release: &str,
    crab: &Octocrab,
) -> Result<Vec<Commit<'c>>, ()> {
    async fn get_100_commits<'c>(
        config: &'c Config,
        date_last_release: &str,
        until_date: DateTime<FixedOffset>,
        crab: &Octocrab,
    ) -> Result<(Vec<Commit<'c>>, DateTime<FixedOffset>), ()> {
        let owner = &config.owner;
        let repo = &config.repo;
        let until_date = &until_date.to_rfc3339();
        let query = format!(
            r#"
    query {{
      repository(owner:"{owner}", name:"{repo}") {{
        refs(refPrefix:"refs/heads/", query:"main", last:1) {{
          edges {{
            node {{
              target {{
                ... on Commit {{
                  history(since:"{date_last_release}", until: "{until_date}") {{
                    edges {{
                      node {{
                        author {{
                          user {{
                            login
                          }}
                        }}
                        message
                        authoredDate
                      }}
                    }}
                  }}
                }}
              }}
            }}
          }}
        }}
      }}
    }}
            "#,
        );
        let response_object: Value = crab
            .graphql(&query)
            .await
            .expect("fetching commits failed");
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
            .flat_map(|obj| config.build_commit(obj).ok())
            .collect();
        Ok((res, new_date))
    }
    // each query only gets 100 commits, so we need to do it in batches of 100
    // we use the date of the last one we received to paginate
    let now = Utc::now();
    let now = now.with_timezone(&FixedOffset::east(0));

    let (mut ret, mut until_date) = get_100_commits(config, date_last_release, now, crab).await?;
    loop {
        let res = get_100_commits(config, date_last_release, until_date, crab).await;
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
pub struct PR<'c> {
    pub commit: Commit<'c>,
    pub kind: PRKind,
}

impl Ord for PR<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.commit.cmp(&other.commit)
    }
}

impl PartialOrd for PR<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.commit.partial_cmp(&other.commit)
    }
}

pub async fn pr_from_commit<'c>(commit: Commit<'c>, crab: &Octocrab) -> Result<PR<'c>, ()> {
    // TODO: get mapping from config
    let mapping: HashMap<&str, PRKind> = HashMap::from([
        ("enhancement", PRKind::Feature),
        ("bug", PRKind::BugFix),
        ("Internal", PRKind::Internal),
    ]);
    let owner = &commit.config.owner;
    let repo = &commit.config.repo;
    let pr_number = &commit.pr_number;
    let query = format!(
        r#"
query {{
  repository(owner:"{owner}", name:"{repo}") {{
    pullRequest(number:{pr_number}) {{
      labels(first: 10) {{
        edges {{
          node {{
            name
          }}
        }}
      }}
    }}
  }}
}}"#,
    );
    let response_object: serde_json::Value = crab
        .graphql(&query)
        .await
        .expect("fetching labels failed");
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

pub fn eprint_ignored(ignored: &BinaryHeap<PR>) {
    eprintln!("-- ignored commits --");
    for pr in ignored {
        let author = &pr.commit.author;
        let number = &pr.commit.pr_number;
        let message = &pr.commit.message;
        eprintln!("#{number} @{author}: {message}");
    }
}
