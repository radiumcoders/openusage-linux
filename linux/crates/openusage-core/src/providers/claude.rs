//! Port of `Sources/OpenUsage/Providers/<Name>/` and its sign-in. Not ported yet.

use crate::kind::ProviderKind;
use crate::provider::{Login, LoginMethod, Provider};

pub fn provider() -> Box<dyn Provider> {
    Box::new(super::NotPorted(ProviderKind::Claude))
}

pub fn login(_method: &LoginMethod) -> Box<dyn Login> {
    Box::new(super::NotPorted(ProviderKind::Claude))
}
