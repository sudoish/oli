//! ChatGPT subscription login and logout commands.

use crate::auth::login::{LoginOptions, provision_config, run as run_login, run_paste};
use crate::auth::store::AuthStore;
use crate::auth::{ISSUER, client_id};
use crate::error::Result;

/// Inputs to the ChatGPT subscription login flow.
#[derive(Debug, Default)]
pub struct Options {
    pub check: bool,
    pub no_browser: bool,
    pub device_auth: bool,
    pub paste: bool,
    pub no_config: bool,
}

/// Run the ChatGPT subscription login flow and optionally update config.
pub async fn login(options: Options) -> Result<()> {
    let store = AuthStore::default_location()?;
    if options.check {
        println!("{}", crate::auth::login::run_release_check(&store).await?);
        return Ok(());
    }
    let login_options = LoginOptions {
        open_browser: !options.no_browser,
        ..LoginOptions::default()
    };
    if options.device_auth {
        crate::auth::device::run(ISSUER, &client_id(), &store).await?;
    } else if options.paste {
        run_paste(&login_options, &store).await?;
    } else {
        run_login(&login_options, &store).await?;
    }

    if options.no_config {
        println!(
            "\nCredentials saved. --no-config was set, so config.toml is unchanged; \n\
             point a provider at them with `kind = \"openai-chatgpt\"`."
        );
    } else {
        provision_config(&store).await?;
    }
    Ok(())
}

/// Remove stored ChatGPT subscription credentials.
pub fn logout() -> Result<()> {
    let store = AuthStore::default_location()?;
    let existed = store.exists();
    store.clear()?;
    if existed {
        println!("Removed {}.", store.path().display());
    } else {
        println!(
            "No stored ChatGPT credentials at {}.",
            store.path().display()
        );
    }
    Ok(())
}
