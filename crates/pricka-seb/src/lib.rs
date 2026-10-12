mod accounts;
mod client;
mod login;
mod transactions;

use async_trait::async_trait;
use client::BrowserSession;
use login::SessionInfo;
use pricka_core::{
    Account, AuthPresenter, Balance, Bank, DateRange, PrickaError, ProxyConfig, Result, Transaction,
};

pub struct SebForetag {
    proxy: ProxyConfig,
    headless: bool,
    session: Option<BrowserSession>,
    info: Option<SessionInfo>,
}

impl SebForetag {
    pub fn new(proxy: ProxyConfig, headless: bool) -> Self {
        Self {
            proxy,
            headless,
            session: None,
            info: None,
        }
    }

    fn page(&self) -> Result<&chromiumoxide::page::Page> {
        self.session
            .as_ref()
            .map(|s| &s.page)
            .ok_or(PrickaError::SessionExpired)
    }

    fn info(&self) -> Result<&SessionInfo> {
        self.info.as_ref().ok_or(PrickaError::SessionExpired)
    }
}

// async-trait marks generated futures as must_use; suppress the duplicate Clippy lint.
#[allow(clippy::double_must_use)]
#[async_trait]
impl Bank for SebForetag {
    fn name(&self) -> &str {
        "SEB Företag"
    }

    async fn authenticate(
        &mut self,
        _personnummer: &str,
        presenter: &dyn AuthPresenter,
    ) -> Result<()> {
        let session = BrowserSession::launch(&self.proxy, self.headless).await?;
        self.session = Some(session);

        let page = self.page()?;
        let info = login::login(page, presenter).await?;
        self.info = Some(info);
        Ok(())
    }

    async fn list_accounts(&self) -> Result<Vec<Account>> {
        accounts::list_accounts(self.page()?, self.info()?).await
    }

    async fn get_transactions(
        &self,
        account_id: &str,
        range: &DateRange,
    ) -> Result<Vec<Transaction>> {
        transactions::get_transactions(self.page()?, self.info()?, account_id, range).await
    }

    async fn get_balance(&self, _account_id: &str) -> Result<Balance> {
        Err(PrickaError::NotImplemented(
            "SEB balance not yet implemented".into(),
        ))
    }

    async fn logout(&mut self) -> Result<()> {
        self.info = None;
        if let Some(session) = self.session.take() {
            session.close().await;
        }
        Ok(())
    }
}

pub fn create(proxy: ProxyConfig, headless: bool) -> Box<dyn Bank> {
    Box::new(SebForetag::new(proxy, headless))
}
