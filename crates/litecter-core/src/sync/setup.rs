//! The hand-offs: what we hand a user so they can deploy a backend we cannot.
//!
//! Litecter holds the token to its own backup and nothing else — no Cloudflare
//! credentials, no way to list an account, no way to deploy. So every route
//! here is *instructions plus verification from our side*, and the instructions
//! are the product. They are generated in one place so that the CLI, the setup
//! wizard and the update dialog cannot quietly disagree about what to do.
//!
//! Three routes, because they are for three different people:
//!
//! | route | for | needs |
//! |---|---|---|
//! | [`Route::Browser`] | anyone | a browser, nothing installed |
//! | [`Route::Agent`] | anyone with a coding agent | an agent with shell access |
//! | [`Route::Terminal`] | developers | node + wrangler |
//!
//! The browser route is the easy one to skip and the most important one to
//! keep: it is the only route that works with nothing installed, and it is the
//! fastest for everyone else too.
//!
//! Two rules run through all of it, both learned from the ways this goes wrong:
//!
//! **Verify before you mutate.** A deploy under a name that does not exist does
//! not error — it silently creates a *second* worker while the real one keeps
//! serving old code. A deploy under a name that belongs to something else
//! silently overwrites it. Where we guessed a name, we say we guessed, the
//! agent is told to ask rather than invent, and the script refuses to run.
//!
//! **Name the specific failure.** "Be careful" is not actionable. Every prompt
//! says what a wrong outcome looks like concretely and gives the command that
//! undoes it.

use super::worker;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Browser,
    Agent,
    Terminal,
}

impl Route {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "browser" | "dashboard" => Some(Self::Browser),
            "agent" | "ai" => Some(Self::Agent),
            "terminal" | "cli" | "shell" => Some(Self::Terminal),
            _ => None,
        }
    }

    /// Whether the text this route produces contains the user's token. Shown at
    /// the copy button, because someone about to paste this into a chat window
    /// deserves to know which one carries a secret.
    pub fn setup_carries_secret(self) -> bool {
        matches!(self, Self::Agent | Self::Terminal)
    }
}

/// What we know — or are guessing — about an existing deployment.
pub struct Deployment {
    pub name: worker::WorkerName,
    pub bucket: String,
    pub endpoint: String,
}

impl Deployment {
    pub fn from_endpoint(endpoint: &str) -> Self {
        Self {
            name: worker::name_from_endpoint(endpoint),
            bucket: worker::DEFAULT_BUCKET_NAME.to_string(),
            endpoint: endpoint.trim_end_matches('/').to_string(),
        }
    }

    /// The deployment setup is about to create. It has no endpoint yet — that
    /// is the thing setup produces — so it takes the names we suggest and the
    /// free `workers.dev` hostname, which needs no domain and no configuration.
    pub fn fresh() -> Self {
        Self {
            name: worker::WorkerName {
                name: worker::DEFAULT_WORKER_NAME.to_string(),
                guessed: false,
            },
            bucket: worker::DEFAULT_BUCKET_NAME.to_string(),
            endpoint: String::new(),
        }
    }

    fn host(&self) -> &str {
        worker::host_of(&self.endpoint)
    }

    /// Whether this deployment needs a `[[routes]]` block, or just the free
    /// hostname. A deployment that does not exist yet will be on workers.dev,
    /// and so — for practical purposes — is one served from localhost by
    /// `wrangler dev`, which has no domain to route at all.
    fn is_workers_dev(&self) -> bool {
        let host = self.host();
        self.endpoint.is_empty()
            || host.ends_with(".workers.dev")
            || matches!(host, "localhost" | "127.0.0.1")
    }

    fn wrangler_toml(&self) -> String {
        self.wrangler_toml_for(&self.name.name, &self.bucket)
    }

    /// The config a redeploy needs, with the name and bucket supplied so the
    /// shell routes can substitute their own variables.
    ///
    /// Taking them as arguments rather than patching the rendered string is not
    /// fussiness: `bucket_name = "litecter-sync"` *contains*
    /// `name = "litecter-sync"`, so a search-and-replace for the worker name
    /// silently rewrote the bucket line too, and the generated script pointed
    /// the backup at a bucket named after the worker.
    ///
    /// The block always restates the bucket binding and any custom domain,
    /// because wrangler treats this file as the whole truth: deploying with a
    /// config that omits the binding *removes* the binding. Secrets are the
    /// exception — they live outside the file and survive untouched, which is
    /// what keeps the update path free of anything sensitive.
    fn wrangler_toml_for(&self, name: &str, bucket: &str) -> String {
        let routing = if self.is_workers_dev() {
            "workers_dev = true".to_string()
        } else {
            format!(
                "workers_dev = false\n\n[[routes]]\npattern = \"{}\"\ncustom_domain = true",
                self.host()
            )
        };
        format!(
            "name = \"{name}\"\n\
             main = \"{asset}\"\n\
             compatibility_date = \"2026-08-01\"\n\
             {routing}\n\
             \n\
             [[r2_buckets]]\n\
             binding = \"{binding}\"\n\
             bucket_name = \"{bucket}\"\n",
            asset = worker::ASSET_NAME,
            binding = worker::BUCKET_BINDING,
        )
    }
}

// ---- setup ---------------------------------------------------------------------

pub fn setup(route: Route, token: &str) -> String {
    match route {
        Route::Browser => browser_setup().to_string(),
        Route::Agent => agent_setup(token),
        Route::Terminal => terminal_setup(token),
    }
}

fn browser_setup() -> &'static str {
    "1.  Open dash.cloudflare.com → R2 → Create bucket. Name it `litecter-sync`.\n\
     \n\
     2.  Workers & Pages → Create → Start with Hello World! → Deploy.\n\
     \x20   Name it `litecter-sync` too.\n\
     \n\
     3.  Edit code. Select everything already in the editor, delete it, and\n\
     \x20   paste in the worker code. Deploy.\n\
     \n\
     4.  Settings → Bindings → Add → R2 bucket.\n\
     \x20   Variable name `SYNC`, bucket `litecter-sync`. Deploy.\n\
     \n\
     5.  Settings → Variables and Secrets → Add → type Secret.\n\
     \x20   Name `SYNC_TOKEN`, value the token. Deploy.\n\
     \n\
     6.  Copy the Worker's URL — it ends in .workers.dev — and paste it below."
}

fn agent_setup(token: &str) -> String {
    format!(
        "Deploy Litecter's backup backend into my Cloudflare account: one Worker in front of one \
R2 bucket. It stores a single encrypted file. Do not change anything else in the account.\n\
\n\
1. Get the worker source. It is one file with no dependencies and no build step:\n\
\n\
   curl -fsSLO {release}\n\
\n\
   If that 404s, clone {repo} and use `worker/src/index.js` instead (rename it to \
{asset}).\n\
\n\
2. Check my credentials:\n\
\n\
   npx wrangler whoami\n\
\n\
   If I am not logged in, run `npx wrangler login` and ask me to finish the sign-in in the \
browser window it opens. If R2 has never been used on this account, it needs a one-time \
activation at dash.cloudflare.com → R2 — pause and ask me to do that too.\n\
\n\
3. Look before you create. Run:\n\
\n\
   npx wrangler r2 bucket list\n\
\n\
   If a bucket named `{bucket}` already exists, stop and ask me. It probably belongs to another \
Litecter install, and pointing a second Worker at it would make two setups overwrite each \
other's backup. Do not reuse it without my say-so.\n\
\n\
4. Write `wrangler.toml` next to the worker file, exactly this:\n\
\n\
{toml}\n\
\n\
5. Create the bucket and deploy:\n\
\n\
   npx wrangler r2 bucket create {bucket}\n\
   npx wrangler deploy\n\
\n\
   Watch the name in the output. Deploying under a name that does not exist does not fail — it \
silently creates a second Worker while nothing tells you. If what got deployed is not called \
`{name}`, remove it with `npx wrangler delete --name <wrong-name>` and try again.\n\
\n\
6. Set the shared secret. This is the token my app will authenticate with — it is the one \
secret in this whole setup:\n\
\n\
   printf '%s' '{token}' | npx wrangler secret put {secret} --name {name}\n\
\n\
7. Verify it yourself before telling me it worked:\n\
\n\
   curl -s -H 'Authorization: Bearer {token}' <the deployed URL>/v1/meta\n\
\n\
   That must print something like {{\"version\":1,\"features\":[\"blob\",\"meta\"]}}. A 503 means \
the secret did not take; a 401 means it took but does not match.\n\
\n\
Then print exactly this line, filled in, and nothing else after it:\n\
\n\
   ENDPOINT: https://...\n\
\n\
Do not commit `wrangler.toml` anywhere, and do not create or modify any other resources in my \
account.",
        release = worker::RELEASE_URL,
        repo = worker::REPO_URL,
        asset = worker::ASSET_NAME,
        bucket = worker::DEFAULT_BUCKET_NAME,
        name = worker::DEFAULT_WORKER_NAME,
        secret = worker::TOKEN_SECRET_NAME,
        toml = indent(&Deployment::fresh().wrangler_toml()),
    )
}

fn terminal_setup(token: &str) -> String {
    format!(
        "# One Worker in front of one R2 bucket, in your own Cloudflare account.\n\
         # Everything below fits inside the free plan.\n\
         \n\
         curl -fsSLO {release}\n\
         \n\
         cat > wrangler.toml <<'TOML'\n\
         {toml}\
         TOML\n\
         \n\
         npx wrangler r2 bucket create {bucket}\n\
         npx wrangler deploy\n\
         \n\
         # The token below is this machine's key to its own backup. Treat it\n\
         # like a password. printf, not echo — a trailing newline in the secret\n\
         # is a classic way to spend an evening debugging a 401.\n\
         printf '%s' '{token}' | npx wrangler secret put {secret} --name {name}\n\
         \n\
         # `wrangler deploy` prints the URL. Paste it into Litecter.\n\
         # Check it first if you like:\n\
         #   curl -s -H 'Authorization: Bearer {token}' <url>/v1/meta",
        release = worker::RELEASE_URL,
        bucket = worker::DEFAULT_BUCKET_NAME,
        name = worker::DEFAULT_WORKER_NAME,
        secret = worker::TOKEN_SECRET_NAME,
        toml = Deployment::fresh().wrangler_toml(),
    )
}

// ---- update --------------------------------------------------------------------

/// Instructions for moving an existing deployment forward.
///
/// `needs_token_secret` covers deployments made before the token check existed:
/// updating their code alone takes them from working to 503, so that one case
/// earns an extra step instead of a mystery outage.
pub fn update(route: Route, d: &Deployment, needs_token_secret: bool) -> String {
    match route {
        Route::Browser => browser_update(d, needs_token_secret),
        Route::Agent => agent_update(d, needs_token_secret),
        Route::Terminal => terminal_update(d, needs_token_secret),
    }
}

fn browser_update(d: &Deployment, needs_token_secret: bool) -> String {
    let which = if d.name.guessed {
        format!(
            "the Worker serving {endpoint} — Litecter cannot tell what it is called from a \
             custom domain, so find it by its route rather than by name",
            endpoint = d.endpoint
        )
    } else {
        format!("the Worker named `{}`", d.name.name)
    };
    let secret_step = if needs_token_secret {
        format!(
            "\n\n4.  This deployment predates Litecter's token check, so it has no secret yet.\n\
             \x20   Settings → Variables and Secrets → Add → type Secret.\n\
             \x20   Name `{secret}`, value the token shown above. Deploy.",
            secret = worker::TOKEN_SECRET_NAME
        )
    } else {
        String::new()
    };
    format!(
        "This is the route to prefer. Replacing only the code leaves the bucket binding, the \
         secret and any custom domain exactly as they are — nothing to re-configure, nothing to \
         re-key.\n\
         \n\
         1.  Open dash.cloudflare.com → Workers & Pages and open {which}.\n\
         \n\
         2.  Edit code. Select everything, delete it, and paste the worker code\n\
         \x20   copied above.\n\
         \n\
         3.  Deploy.{secret_step}\n\
         \n\
         Then press Check again below. Your backups keep working throughout."
    )
}

fn agent_update(d: &Deployment, needs_token_secret: bool) -> String {
    let naming = if d.name.guessed {
        format!(
            "Litecter guessed the name `{name}` from the endpoint {endpoint} — a custom domain \
does not reveal what the Worker is called, so this may well be wrong. If no Worker by that name \
exists, ask me which one it is. Do not substitute a name you invented: deploying under a name \
that does not exist silently creates a second Worker while the real one keeps serving old code, \
and deploying over a name that belongs to something else destroys it.",
            name = d.name.name,
            endpoint = d.endpoint
        )
    } else {
        format!(
            "The Worker is called `{name}` — that is read from the endpoint, not guessed. Still \
confirm it exists before deploying: a deploy under a name that does not exist silently creates a \
second Worker rather than failing.",
            name = d.name.name
        )
    };
    let secret_step = if needs_token_secret {
        format!(
            "\n\n6. This deployment predates the token check and has no `{secret}` secret, so \
after deploying it will answer 503 until one is set. Tell me that, and ask me for the token from \
Litecter → Settings → Backup. Do not invent one.\n",
            secret = worker::TOKEN_SECRET_NAME
        )
    } else {
        String::new()
    };
    format!(
        "Update an existing Cloudflare Worker of mine to newer code. Only the code changes: its \
R2 bucket binding, its secrets and its domain must all survive. This prompt contains no \
credentials.\n\
\n\
1. Get the new source — one file, no build step:\n\
\n\
   curl -fsSLO {release}\n\
\n\
   If that 404s, clone {repo} and use `worker/src/index.js` (renamed to {asset}).\n\
\n\
2. Check my credentials:\n\
\n\
   npx wrangler whoami\n\
\n\
   If I am not logged in, run `npx wrangler login` and ask me to finish the sign-in in the \
browser it opens.\n\
\n\
3. Confirm what you are about to overwrite before you overwrite it:\n\
\n\
   npx wrangler deployments list --name {name}\n\
\n\
   {naming}\n\
\n\
4. Write `wrangler.toml` next to the file. It must restate the bucket binding — wrangler treats \
this file as the whole truth, so a config that omits the binding removes it:\n\
\n\
{toml}\n\
\n\
   Litecter assumes the bucket is called `{bucket}` but has no way to see it. Check with \
`npx wrangler r2 bucket list` and correct `bucket_name` if it differs — deploying with the wrong \
bucket points my backups at an empty one, and the old backup is orphaned rather than deleted, so \
nothing will look broken.\n\
\n\
5. Deploy:\n\
\n\
   npx wrangler deploy\n\
{secret_step}\
\n\
Then tell me it is done and I will re-check from the app. Do not commit `wrangler.toml` \
anywhere, and do not create or modify any other resources in my account.",
        release = worker::RELEASE_URL,
        repo = worker::REPO_URL,
        asset = worker::ASSET_NAME,
        name = d.name.name,
        bucket = d.bucket,
        toml = indent(&d.wrangler_toml()),
    )
}

fn terminal_update(d: &Deployment, needs_token_secret: bool) -> String {
    // The script cannot ask a question, so where the name is a guess it refuses
    // rather than gambling on it — and says which line to correct.
    let guard = if d.name.guessed {
        "# Litecter guessed this name from a custom domain and may be wrong.\n\
         # Check it, correct the line above if needed, then delete this block.\n\
         npx wrangler deployments list --name \"$WORKER\" >/dev/null 2>&1 || {\n\
         \x20 echo \"No Worker called $WORKER. Set WORKER= above to the right name.\" >&2\n\
         \x20 exit 1\n\
         }\n\n"
            .to_string()
    } else {
        String::new()
    };
    let secret_note = if needs_token_secret {
        format!(
            "\n# This deployment predates the token check, so after deploying it will\n\
             # answer 503 until you set the secret. The token is in Litecter →\n\
             # Settings → Backup:\n\
             #   printf '%s' '<token>' | npx wrangler secret put {secret} --name \"$WORKER\"\n",
            secret = worker::TOKEN_SECRET_NAME
        )
    } else {
        String::new()
    };
    format!(
        "# Updates the code only. Secrets live outside wrangler.toml and survive,\n\
         # which is why nothing below is sensitive.\n\
         \n\
         WORKER={name}\n\
         BUCKET={bucket}   # `npx wrangler r2 bucket list` if you are unsure\n\
         \n\
         {guard}\
         curl -fsSLO {release}\n\
         \n\
         cat > wrangler.toml <<TOML\n\
         {toml}\
         TOML\n\
         \n\
         npx wrangler deploy\n\
         {secret_note}",
        name = d.name.name,
        bucket = d.bucket,
        release = worker::RELEASE_URL,
        // Unquoted heredoc above, so these expand — letting someone correct a
        // guessed name in one place rather than three.
        toml = d.wrangler_toml_for("$WORKER", "$BUCKET"),
    )
}

fn indent(block: &str) -> String {
    block.lines().map(|l| format!("   {l}")).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "aa11bb22cc33dd44ee55ff6600778899aa11bb22cc33dd44ee55ff6600778899";

    fn workers_dev() -> Deployment {
        Deployment::from_endpoint("https://litecter-sync.alice.workers.dev")
    }

    fn custom_domain() -> Deployment {
        Deployment::from_endpoint("https://sync.example.com")
    }

    #[test]
    fn setup_says_which_routes_carry_the_token() {
        // The user is told at the copy button. If this ever flips, the label
        // flips with it — a prompt that silently starts carrying a secret is
        // exactly the thing that label exists to prevent.
        for route in [Route::Agent, Route::Terminal] {
            assert!(setup(route, TOKEN).contains(TOKEN));
            assert!(route.setup_carries_secret());
        }
        assert!(!setup(Route::Browser, TOKEN).contains(TOKEN));
        assert!(!Route::Browser.setup_carries_secret());
    }

    #[test]
    fn no_update_route_carries_the_token() {
        // Update instructions get pasted into agents and chat windows far more
        // casually than setup ones, and they never need a secret: a same-name
        // redeploy leaves it untouched.
        for route in [Route::Browser, Route::Agent, Route::Terminal] {
            for needs_secret in [false, true] {
                let text = update(route, &workers_dev(), needs_secret);
                assert!(!text.contains(TOKEN), "{route:?} leaked the token");
            }
        }
    }

    #[test]
    fn a_redeploy_config_always_restates_the_bucket_binding() {
        // Omitting it does not error — it removes the binding, and the next
        // sync fails against a Worker with nowhere to write.
        for d in [workers_dev(), custom_domain()] {
            let toml = d.wrangler_toml();
            assert!(toml.contains("[[r2_buckets]]"), "{toml}");
            assert!(toml.contains(&format!("binding = \"{}\"", worker::BUCKET_BINDING)));
        }
    }

    #[test]
    fn a_custom_domain_is_carried_into_the_redeploy_config() {
        let toml = custom_domain().wrangler_toml();
        assert!(toml.contains("custom_domain = true"), "{toml}");
        assert!(toml.contains("pattern = \"sync.example.com\""), "{toml}");
        assert!(toml.contains("workers_dev = false"), "{toml}");

        let toml = workers_dev().wrangler_toml();
        assert!(toml.contains("workers_dev = true"), "{toml}");
        assert!(!toml.contains("[[routes]]"), "{toml}");
    }

    #[test]
    fn a_guessed_name_is_flagged_everywhere_it_appears() {
        let d = custom_domain();
        assert!(d.name.guessed);
        assert!(update(Route::Agent, &d, false).contains("guessed"));
        assert!(update(Route::Browser, &d, false).contains("cannot tell"));
        // The script has nobody to ask, so it must refuse rather than gamble.
        assert!(update(Route::Terminal, &d, false).contains("exit 1"));
    }

    #[test]
    fn the_script_keeps_the_bucket_separate_from_the_worker() {
        // The regression: `bucket_name = "litecter-sync"` contains
        // `name = "litecter-sync"`, so patching the rendered config to insert
        // $WORKER rewrote the bucket line too. The script then deployed a
        // backend pointed at a bucket named after the worker — which silently
        // starts the backup over instead of failing.
        let text = update(Route::Terminal, &workers_dev(), false);
        assert!(text.contains("name = \"$WORKER\""), "{text}");
        assert!(text.contains("bucket_name = \"$BUCKET\""), "{text}");
        assert!(!text.contains("bucket_name = \"$WORKER\""), "bucket took the worker's name");
    }

    #[test]
    fn the_secret_hint_is_a_command_you_can_actually_run() {
        // `%%` is a format escape in some languages and not in Rust's, so it
        // leaked into the generated shell verbatim.
        let text = update(Route::Terminal, &workers_dev(), true);
        assert!(text.contains("printf '%s'"), "{text}");
        assert!(!text.contains("%%"), "a format escape leaked into the shell:\n{text}");
    }

    #[test]
    fn a_local_dev_backend_is_not_given_a_custom_domain() {
        // `wrangler dev` serves on localhost, which is not a domain anyone can
        // route — emitting one produces a config that cannot deploy.
        let toml = Deployment::from_endpoint("http://localhost:8787").wrangler_toml();
        assert!(toml.contains("workers_dev = true"), "{toml}");
        assert!(!toml.contains("[[routes]]"), "{toml}");
    }

    #[test]
    fn a_known_name_does_not_make_the_script_refuse_to_run() {
        let text = update(Route::Terminal, &workers_dev(), false);
        assert!(!text.contains("exit 1"), "a certain name needs no guard:\n{text}");
        assert!(text.contains("WORKER=litecter-sync"));
    }

    #[test]
    fn the_pre_versioning_case_gets_its_extra_step() {
        for route in [Route::Browser, Route::Agent, Route::Terminal] {
            let with = update(route, &workers_dev(), true);
            let without = update(route, &workers_dev(), false);
            assert!(with.contains(worker::TOKEN_SECRET_NAME), "{route:?}");
            assert!(!without.contains(worker::TOKEN_SECRET_NAME), "{route:?}");
        }
    }

    #[test]
    fn setup_configures_the_deployment_it_tells_you_to_create() {
        // The regression: the config was built from a placeholder endpoint, so
        // it told people to create `litecter-sync` and then deployed a worker
        // called `x`. Every name in the instructions has to be the same name.
        for route in [Route::Agent, Route::Terminal] {
            let text = setup(route, TOKEN);
            assert!(
                text.contains(&format!("name = \"{}\"", worker::DEFAULT_WORKER_NAME)),
                "{route:?} config disagrees with its own instructions:\n{text}"
            );
            assert!(text.contains("workers_dev = true"), "{route:?} should need no domain");
            assert!(!text.contains("[[routes]]"), "{route:?} should not route a domain");
        }
    }

    #[test]
    fn every_route_points_at_the_stable_download() {
        for text in [setup(Route::Agent, TOKEN), setup(Route::Terminal, TOKEN)] {
            assert!(text.contains(worker::RELEASE_URL));
        }
    }

    #[test]
    fn route_names_users_might_type_all_resolve() {
        assert_eq!(Route::parse("Browser"), Some(Route::Browser));
        assert_eq!(Route::parse("dashboard"), Some(Route::Browser));
        assert_eq!(Route::parse(" agent "), Some(Route::Agent));
        assert_eq!(Route::parse("cli"), Some(Route::Terminal));
        assert_eq!(Route::parse("carrier pigeon"), None);
    }
}
