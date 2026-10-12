mod accounts;
mod client;
pub mod corporate;
mod corporate_login;
mod login;
mod transactions;

use async_trait::async_trait;
use client::BrowserSession;
use login::SessionTokens;
use pricka_core::{
    Account, AuthPresenter, Balance, Bank, DateRange, PrickaError, ProxyConfig, Result, Transaction,
};

pub struct Handelsbanken {
    proxy: ProxyConfig,
    headless: bool,
    session: Option<BrowserSession>,
    tokens: Option<SessionTokens>,
}

impl Handelsbanken {
    pub fn new(proxy: ProxyConfig, headless: bool) -> Self {
        Self {
            proxy,
            headless,
            session: None,
            tokens: None,
        }
    }

    fn tokens(&self) -> Result<&SessionTokens> {
        self.tokens.as_ref().ok_or(PrickaError::SessionExpired)
    }

    fn page(&self) -> Result<&chromiumoxide::page::Page> {
        self.session
            .as_ref()
            .map(|s| &s.page)
            .ok_or(PrickaError::SessionExpired)
    }
}

// async-trait marks generated futures as must_use; suppress the duplicate Clippy lint.
#[allow(clippy::double_must_use)]
#[async_trait]
impl Bank for Handelsbanken {
    fn name(&self) -> &str {
        "Handelsbanken Privat"
    }

    async fn authenticate(
        &mut self,
        personnummer: &str,
        presenter: &dyn AuthPresenter,
    ) -> Result<()> {
        // Launch browser lazily on first authenticate call
        let session = BrowserSession::launch(&self.proxy, self.headless).await?;
        self.session = Some(session);

        let page = self.page()?;
        let tokens = login::login(page, personnummer, presenter).await?;
        self.tokens = Some(tokens);
        Ok(())
    }

    async fn list_accounts(&self) -> Result<Vec<Account>> {
        accounts::list_accounts(self.page()?, self.tokens()?).await
    }

    async fn get_transactions(
        &self,
        account_id: &str,
        range: &DateRange,
    ) -> Result<Vec<Transaction>> {
        transactions::get_transactions(self.page()?, self.tokens()?, account_id, range).await
    }

    async fn get_balance(&self, _account_id: &str) -> Result<Balance> {
        Err(PrickaError::NotImplemented(
            "Balance endpoint not yet captured".into(),
        ))
    }

    async fn logout(&mut self) -> Result<()> {
        self.tokens = None;
        if let Some(session) = self.session.take() {
            session.close().await;
        }
        Ok(())
    }
}

/// Factory function for creating a Handelsbanken Privat adapter.
pub fn create(proxy: ProxyConfig, headless: bool) -> Box<dyn Bank> {
    Box::new(Handelsbanken::new(proxy, headless))
}
