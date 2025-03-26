# Octocrab Project Review

Found project by asking chatGPT for a few open source rust projects on github given the goal of finding areas that are good and in need of improvement. It suggested

- Octocrab: A modern, extensible GitHub API client for Rust
- Progenitor: A Rust crate for generating opinionated clients from OpenAPI 3.0.x specifications
- Yaak: An intuitive desktop API client supporting REST, GraphQL, WebSockets, Server-Sent Events, and gRPC

I chose Octocrab because it's pretty straightforward and familiar enough in structure and dependencies that I could grok it quickly.

## Octocrab

Octocrab is a modern, extensible GitHub API client written in Rust. The project is structured as a Rust library crate with comprehensive documentation, [examples](./examples), and a modular architecture that separates different GitHub API domains into distinct modules.

## Methodology

I prompted windsurf and chatgpt throughout this experience to identify areas that were good and in need of improvement. For those needing improvement I tried to have windsurf implement code for me, with mixed results.

## Well Written Areas

### Builder Pattern Implementation

The project implements the builder pattern throughout its codebase, allowing for flexible and intuitive API construction. This is particularly evident in how it handles authentication and client configuration, making it easy for users to customize their GitHub API interactions while maintaining clean, readable code.

### API Layer Abstraction

Octocrab demonstrates excellent architecture in its dual-layer API approach:

- High-level semantic API providing strongly typed interfaces for common GitHub operations
- Low-level HTTP API allowing direct request customization and extension

This separation allows users to choose the appropriate level of abstraction for their needs while maintaining a consistent interface.

### Middleware and Service Architecture

The project makes excellent use of Tower services and middleware patterns, allowing for:

- Custom request/response processing
- Rate limiting/retry handling
- Authentication middleware
- Extensible error handling

This architecture makes the library both robust and flexible for various use cases.

### Example Code

```rust
use octocrab::Octocrab;

// as long as [`src/from_response.rs`] is in scope, its blanket impl over DeserializeOwned will work
#[derive(serde::Deserialize)]
struct CustomUser {
    id: u64,
}

async fn bulider() -> octocrab::Result<()> {
    let token = std::env::var("GITHUB_TOKEN").expect("GITHUB_TOKEN env variable is required");

    // builder
    let octocrab = Octocrab::builder().personal_token(token).build()?;

    let repo = octocrab.repos("rust-lang", "rust").get().await?;

    // strong typing
    let repo_metrics = octocrab
        .repos("rust-lang", "rust")
        .get_community_profile_metrics()
        .await?;

    // low level for unsupported routes with custom response
    let user: CustomUser = octocrab.get("/user", None::<&()>).await?;

    println!(
        "{} has {} stars and {}% health percentage",
        repo.full_name.unwrap(),
        repo.stargazers_count.unwrap_or(0),
        repo_metrics.health_percentage
    );

    Ok(())
}

```

## Improved Areas

### Manual type definitions

The project defines many types manually, which allows for fine grained control and the potential for a more ergonomic API, it's probably worth considering using a code generator from the github open API spec to reduce the amount of manual work: [open issue](https://github.com/XAMPPRocky/octocrab/issues/377).

It does include some macros to add behavior to models and webhook events currently. If the openAPI spec was used to generate structs, there would likely need to be some macros (proc/simple) to add the remaining behavior that keeps the existing API interface/ergonomics.

### Changes implemented

```sh
❯ git log
commit 0f2df3274695e7e65091a9765d3d129d503c7aad (HEAD -> main)
Author: David Estes <dav1do@users.noreply.github.com>
Date:   Wed Mar 26 09:03:20 2025 -0600

    chore: collect all fields in webhook payloads in case API is updated before client

commit 038bbbe546455bce571ea93d3bf4b38f5b4867ed
Author: David Estes <dav1do@users.noreply.github.com>
Date:   Wed Mar 26 09:03:20 2025 -0600

    wip: parse webhook responses from http directly

commit c5a9dda71daa6c416780ec9df8ffb7fccd2013e7
Author: David Estes <dav1do@users.noreply.github.com>
Date:   Wed Mar 26 09:03:19 2025 -0600

    feat: respect rate limit data returned when retrying

    parse the rate limit headers returned and pause requests until x-ratelimit-reset value

```

#### Collect all fields in webhook payloads

I noticed comments saying that the webhook payloads were not complete and in beta. As I didn't see any field that was collecting all the unknown fields, I thought it'd be nice to add. I asked windsurf about it and suggested it add the following to the webhook event and payload structs:

```rust
[serde(flatten)]
other: HashMap<String, serde_json::Value>
```

it took a few tries to get it find everything (e.g. look for all structs named `*Payload` in the `models/events/payload` module), but it seems to have managed in the end, including updating tests.

#### Parse webhook responses

I noticed that the webhook event parsing was a bit clunky and asked windsurf to add a `from_response` function to the `WebhookEvent` struct. The existing error types didn't seem to fit this missing header, so it currently panics when there should probably be a new error (e.g. bad input or just use the existing Other variant).

```rust
// Value picked from the `X-GitHub-Event` header
let event_name = "ping";
let event = WebhookEvent::try_from_header_and_body(event_name, json)?;

```

#### Rate limiting

This was the only non-trivial change. After identifying that there was no real handling of rate limits, I prompted windsurf to architect and add I got some pretty good ideas that turned out to be impractical given the time frame/github api, as I had insufficient knowledege about how the github API (as well as how this project was laid out) worked and didn't scope the implementaton correctly at first.

- **FAIL**: Windsurf refactored tons of lints, making it hard to see the changes and broke the build (I disabled them).
- **FAIL**: generated a new middleware (tower service/layer), complete with token rotation, caching in redis/file/memory using a trait, pre-emptive throttling to avoid rate limits
  - not a good way to map route to rate limit "bucket" until told in response
  - impractical to rotate across multiple tokens
- **SUCCESS**: reprompted to add basic rate limit waiting to the existing retry policy
- **SUCCESS**: prompted to write tests for me
  - **FAIL**: added tokio::timeout manually to avoid infinte tests
  - **SUCCESS**: tests failed when all run simultaneously due single threaded runtime -> prompted windsurf about this and it gave good suggestions about using `tokio::time::pause` and `tokio::time::advance` to avoid adding real sleeps
  - **SUCCESS**: prompted windsurf to ensure that the delay was not zero and it actually did sleep and it suggested using `yield_now` to ensure the future was not ready.

