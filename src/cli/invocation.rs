use crate::marketplace::{MarketplaceContext, MarketplaceId};

pub fn tori(command: impl AsRef<str>) -> String {
    qualified("flea tori", command.as_ref())
}

pub fn vinted_fi(command: impl AsRef<str>) -> String {
    qualified("flea vinted --portal fi", command.as_ref())
}

pub fn capabilities(context: MarketplaceContext) -> String {
    match context.marketplace {
        MarketplaceId::Tori => tori("capabilities"),
        MarketplaceId::Vinted => vinted_fi("capabilities"),
    }
}

pub fn marketplaces() -> String {
    "flea marketplaces".to_owned()
}

fn qualified(prefix: &str, command: &str) -> String {
    let command = command.trim();
    if command.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix} {command}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_preserve_explicit_marketplace_context() {
        assert_eq!(tori("search chair"), "flea tori search chair");
        assert_eq!(
            vinted_fi("auth status"),
            "flea vinted --portal fi auth status"
        );
        assert_eq!(
            capabilities(MarketplaceContext::VINTED_FI),
            "flea vinted --portal fi capabilities"
        );
    }
}
