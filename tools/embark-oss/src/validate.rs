use crate::{codeowners::CodeOwners, github, slack};
use eyre::{eyre, WrapErr};
use futures::TryFutureExt;
use itertools::Itertools;
use std::collections::HashSet;

#[derive(Debug)]
struct Project {
    name: String,
    maintainers: eyre::Result<HashSet<String>>,
}

impl Project {
    pub fn new(name: String) -> Self {
        Self {
            name,
            maintainers: not_yet_checked(),
        }
    }

    pub fn from_website_project(project: OpenSourceWebsiteDataProject) -> Self {
        Self::new(project.name)
    }

    pub async fn validate(self) -> Self {
        Project {
            maintainers: lookup_project_maintainers(&self.name).await,
            name: self.name,
        }
    }

    pub fn has_errors(&self) -> bool {
        let Self {
            name: _,
            maintainers,
        } = self;
        maintainers.is_err()
    }

    pub fn errors(&self) -> Vec<&eyre::Report> {
        let Self {
            name: _,
            maintainers,
        } = self;
        vec![maintainers.as_ref().err()]
            .into_iter()
            .flatten()
            .collect()
    }

    pub fn errors_to_string(&self, indent: bool) -> Option<String> {
        let errors = self.errors();
        if errors.is_empty() {
            return None;
        }
        Some(
            errors
                .into_iter()
                .map(|error| crate::error::cause_string(error.as_ref(), indent))
                .join("\n"),
        )
    }
}

fn not_yet_checked<T>() -> eyre::Result<T> {
    Err(eyre!("This property has not yet been validated"))
}

/// Validate all projects listed in the data.json of the Embark Open Source
/// website.
pub async fn all(slack_webhook_url: Option<String>) -> eyre::Result<()> {
    // Download list of projects and download CODEOWNERS file for each one
    let projects = download_projects_list().await?;
    let futures = projects.into_iter().map(|project| project.validate());
    let projects = futures::future::join_all(futures).await;

    // Print results
    projects.iter().for_each(print_status);

    // Collected the projects with issues
    let problem_projects: Vec<_> = projects
        .into_iter()
        .filter(|project| project.has_errors())
        .collect();

    // If there is no problem we are done and can return
    if problem_projects.is_empty() {
        return Ok(());
    }

    // Send a message to slack if a webhook URL has been given
    if let Some(url) = slack_webhook_url {
        let blocks = slack_notification_blocks(problem_projects.as_slice());
        slack::send_webhook(&url, blocks).await?;
    }

    Err(eyre!("Not all projects conform to our guidelines"))
}

/// Validate a single project from the EmbarkStudios GitHub organisation.
pub async fn one(project_name: String) -> eyre::Result<()> {
    let project = Project::new(project_name).validate().await;
    print_status(&project);
    if project.has_errors() {
        Err(eyre!("The project does not conform to our guidelines"))
    } else {
        Ok(())
    }
}

fn print_status(project: &Project) {
    if let Some(errors) = project.errors_to_string(true) {
        return print!("❌ {}\n{}\n", project.name, errors);
    }

    if let Ok(maintainers) = &project.maintainers {
        return println!("✔️ {} ({})", project.name, maintainers.iter().join(", "));
    }

    unreachable!();
}

async fn download_projects_list() -> eyre::Result<Vec<Project>> {
    let data = github::download_repo_json_file::<OpenSourceWebsiteData>(
        "EmbarkStudios",
        "opensource-website",
        "main",
        "data.json",
    )
    .await
    .wrap_err("Unable to get list of open source Embark projects")?;
    Ok(data
        .projects
        .into_iter()
        .map(Project::from_website_project)
        .collect())
}

async fn lookup_project_maintainers(name: &str) -> eyre::Result<HashSet<String>> {
    // Download CODEOWNERS from one of the accepted branches
    let get =
        |branch| github::download_repo_file("EmbarkStudios", name, branch, ".github/CODEOWNERS");
    let text = get("main").or_else(|_| get("master")).await?;

    // Determine if there is at least 1 primary maintainer listed for each project
    CodeOwners::new(&text)
        .wrap_err("Unable to determine maintainers")?
        .primary_maintainers()
        .cloned()
        .ok_or(eyre!("No maintainers were found for * the CODEOWNERS file"))
}

#[derive(Debug, serde::Deserialize)]
pub struct OpenSourceWebsiteData {
    projects: Vec<OpenSourceWebsiteDataProject>,
}
#[derive(Debug, serde::Deserialize)]
pub struct OpenSourceWebsiteDataProject {
    name: String,
}

fn slack_notification_blocks(projects: &[Project]) -> Vec<slack::Block> {
    use slack::Block::{Divider, Text};

    let head = "The following Embark open source projects have been found to \
have maintainership issues.";
    let foot = "This message was generated by the \
<https://github.com/EmbarkStudios/opensource/tree/main/tools/embark-oss|embark-oss tool> \
on GitHub Actions.";

    let mut blocks = Vec::with_capacity(projects.len() + 4);

    blocks.push(Text(head.to_string()));
    blocks.push(Divider);
    blocks.extend(projects.iter().flat_map(slack_project_block));
    blocks.push(Divider);
    blocks.push(Text(foot.to_string()));
    blocks
}

fn slack_project_block(project: &Project) -> Option<slack::Block> {
    let text = format!(
        ":red_circle: *<https://github.com/EmbarkStudios/{name}|{name}>*\n```{error}```",
        name = &project.name,
        error = project.errors_to_string(false)?,
    );
    Some(slack::Block::Text(text))
}
