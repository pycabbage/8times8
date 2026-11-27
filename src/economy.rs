use std::{fmt, str::FromStr};

use num_bigint::BigUint;

use crate::Level;

const STORAGE_BITS: u64 = 4096;
const MAX_DECIMAL_DIGITS: usize = 1234;
// The last stages for which 20 × 3^stage fits in 128 and 4096 bits.
const SMALL_PRICE_STAGE: u32 = 78;
const MAX_PRICE_STAGE: u32 = 2581;

// `Large` is always greater than u128::MAX, so the derived order is numeric.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
enum Magnitude {
    Small(u128),
    Large(BigUint),
}

impl Magnitude {
    fn normalize(value: BigUint) -> Self {
        match u128::try_from(&value) {
            Ok(small) => Self::Small(small),
            Err(_) => Self::Large(value),
        }
    }

    fn bits(&self) -> u64 {
        match self {
            Self::Small(n) => u64::from(u128::BITS - n.leading_zeros()),
            Self::Large(n) => n.bits(),
        }
    }

    fn add(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::Small(a), Self::Small(b)) => match a.checked_add(*b) {
                Some(sum) => Self::Small(sum),
                None => Self::Large(BigUint::from(*a) + *b),
            },
            (Self::Large(a), Self::Small(b)) | (Self::Small(b), Self::Large(a)) => {
                Self::Large(a + *b)
            }
            (Self::Large(a), Self::Large(b)) => Self::Large(a + b),
        }
    }

    fn checked_sub(&self, other: &Self) -> Option<Self> {
        match (self, other) {
            (Self::Small(a), Self::Small(b)) => a.checked_sub(*b).map(Self::Small),
            (Self::Large(a), Self::Small(b)) => Some(Self::normalize(a - *b)),
            (Self::Large(a), Self::Large(b)) if a >= b => Some(Self::normalize(a - b)),
            _ => None,
        }
    }

    fn is_covered_by_sum(&self, left: &Self, right: &Self) -> bool {
        if self <= left || self <= right {
            return true;
        }
        if let (Self::Small(target), Self::Small(a), Self::Small(b)) = (self, left, right) {
            // Compare with the remaining gap, even if a + b would overflow.
            return *b >= target - a;
        }
        *self <= left.add(right)
    }
}

impl fmt::Display for Magnitude {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Small(n) => fmt::Display::fmt(n, f),
            Self::Large(n) => fmt::Display::fmt(n, f),
        }
    }
}

/// An exact, nonnegative integer that fits in the 4096-bit storage limit.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct Amount(Magnitude);

impl Amount {
    pub const ZERO: Self = Self(Magnitude::Small(0));

    pub fn bits(&self) -> u64 {
        self.0.bits()
    }

    /// Returns `None` if the exact sum does not fit in storage.
    pub fn checked_add(&self, other: &Self) -> Option<Self> {
        let sum = self.0.add(&other.0);
        (sum.bits() <= STORAGE_BITS).then_some(Self(sum))
    }

    /// Returns `None` if the difference would be negative.
    pub fn checked_sub(&self, other: &Self) -> Option<Self> {
        self.0.checked_sub(&other.0).map(Self)
    }

    /// The price `20 × 3^stage`, or `None` when it exceeds the storage limit.
    /// In particular, enormous stages are rejected before any exponentiation.
    pub fn machine_price(stage: u32) -> Option<Self> {
        if stage > MAX_PRICE_STAGE {
            return None;
        }
        if stage <= SMALL_PRICE_STAGE {
            return 3u128.checked_pow(stage)?.checked_mul(20).map(Self::from);
        }
        Some(Self(Magnitude::Large(BigUint::from(3u8).pow(stage) * 20u8)))
    }
}

impl Default for Amount {
    fn default() -> Self {
        Self::ZERO
    }
}

impl From<u128> for Amount {
    fn from(value: u128) -> Self {
        Self(Magnitude::Small(value))
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Amount({self})")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AmountParseError {
    InvalidDecimal,
    AboveLimit,
}

impl fmt::Display for AmountParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidDecimal => "amount must contain only ASCII decimal digits",
            Self::AboveLimit => "amount exceeds the decimal input or 4096-bit storage limit",
        })
    }
}

impl std::error::Error for AmountParseError {}

impl FromStr for Amount {
    type Err = AmountParseError;

    /// Accepts 1 through 1234 ASCII digits, including any leading zeroes.
    /// Even at exactly 1234 digits the value must fit in 4096 bits.
    fn from_str(digits: &str) -> Result<Self, Self::Err> {
        if digits.len() > MAX_DECIMAL_DIGITS {
            return Err(AmountParseError::AboveLimit);
        }
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(AmountParseError::InvalidDecimal);
        }
        if let Ok(small) = digits.parse::<u128>() {
            return Ok(Self::from(small));
        }
        let value =
            BigUint::parse_bytes(digits.as_bytes(), 10).ok_or(AmountParseError::InvalidDecimal)?;
        if value.bits() > STORAGE_BITS {
            return Err(AmountParseError::AboveLimit);
        }
        Ok(Self(Magnitude::Large(value)))
    }
}

/// An exact available balance. It can be one bit wider than a stored `Amount`.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct Balance(Magnitude);

impl Balance {
    pub fn bits(&self) -> u64 {
        self.0.bits()
    }

    /// Tests the balance only, without considering the spending storage limit.
    pub fn covers(&self, cost: &Amount) -> bool {
        self.0 >= cost.0
    }
}

impl fmt::Display for Balance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for Balance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Balance({self})")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaymentError {
    InsufficientBalance,
    SpendingLimit,
}

impl fmt::Display for PaymentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InsufficientBalance => "insufficient balance at this level",
            Self::SpendingLimit => "total spending would exceed the 4096-bit storage limit",
        })
    }
}

impl std::error::Error for PaymentError {}

/// The bounded stored values for one level. They are not netted or truncated.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Account {
    cleared: Amount,
    bonus: Amount,
    spent: Amount,
}

impl Account {
    pub fn new(cleared: Amount, bonus: Amount, spent: Amount) -> Self {
        Self {
            cleared,
            bonus,
            spent,
        }
    }

    pub fn cleared(&self) -> &Amount {
        &self.cleared
    }

    pub fn bonus(&self) -> &Amount {
        &self.bonus
    }

    pub fn spent(&self) -> &Amount {
        &self.spent
    }

    pub fn available(&self) -> Balance {
        let total = self.cleared.0.add(&self.bonus.0);
        if self.spent == Amount::ZERO {
            return Balance(total);
        }
        Balance(
            total
                .checked_sub(&self.spent.0)
                .unwrap_or(Magnitude::Small(0)),
        )
    }

    /// Charges this account only. Errors leave every stored value unchanged.
    pub fn try_pay(&mut self, cost: &Amount) -> Result<(), PaymentError> {
        if *cost == Amount::ZERO {
            return Ok(());
        }
        let new_spent = self
            .spent
            .checked_add(cost)
            .ok_or(PaymentError::SpendingLimit)?;
        // For a positive cost, available >= cost iff gross credits >= new_spent.
        if !new_spent
            .0
            .is_covered_by_sum(&self.cleared.0, &self.bonus.0)
        {
            return Err(PaymentError::InsufficientBalance);
        }
        self.spent = new_spent;
        Ok(())
    }
}

/// Eight independent accounts. A payment never draws from another level.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Ledger {
    accounts: [Account; Level::ALL.len()],
}

impl Ledger {
    pub fn account(&self, level: Level) -> &Account {
        &self.accounts[level.index()]
    }

    pub fn set_account(&mut self, level: Level, account: Account) {
        self.accounts[level.index()] = account;
    }

    pub fn try_pay(&mut self, level: Level, cost: &Amount) -> Result<(), PaymentError> {
        self.accounts[level.index()].try_pay(cost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage_max() -> BigUint {
        (BigUint::from(1u8) << 4096usize) - 1u8
    }

    #[test]
    fn decimal_accepts_only_bounded_unsigned_ascii() {
        for bad in ["", "-0", "-1", "+1", " 1", "1\n", "3.0", "2e3", "１"] {
            assert_eq!(bad.parse::<Amount>(), Err(AmountParseError::InvalidDecimal));
        }
        assert_eq!("00042".parse(), Ok(Amount::from(42)));
        assert_eq!("0".repeat(1234).parse(), Ok(Amount::ZERO));
        assert_eq!(
            "0".repeat(1235).parse::<Amount>(),
            Err(AmountParseError::AboveLimit)
        );
    }

    #[test]
    fn decimal_checks_the_value_at_the_maximum_digit_count() {
        let max = storage_max();
        let max_digits = max.to_string();
        let above_max = (max + 1u8).to_string();
        assert_eq!(max_digits.len(), 1234);
        assert_eq!(above_max.len(), 1234);
        let stored: Amount = max_digits.parse().unwrap();
        assert_eq!(stored.bits(), 4096);
        assert_eq!(stored.to_string(), max_digits);
        assert_eq!(
            above_max.parse::<Amount>(),
            Err(AmountParseError::AboveLimit)
        );
    }

    #[test]
    fn small_values_keep_exact_integer_precision_past_2_to_53() {
        let below = "9007199254740991".parse::<Amount>().unwrap();
        let at = "9007199254740992".parse::<Amount>().unwrap();
        let above = "9007199254740993".parse::<Amount>().unwrap();
        assert!(below < at && at < above);
        assert_eq!(below.checked_add(&Amount::from(2)), Some(above.clone()));
        assert_eq!(above.checked_sub(&at), Some(Amount::from(1)));
        assert!(matches!(above.0, Magnitude::Small(_)));
    }

    #[test]
    fn promoting_and_demoting_preserves_comparison_and_decimal_values() {
        let max = Amount::from(u128::MAX);
        let one = Amount::from(1);
        let promoted = max.checked_add(&one).unwrap();
        assert!(matches!(max.0, Magnitude::Small(_)));
        assert!(matches!(promoted.0, Magnitude::Large(_)));
        assert_eq!(
            promoted.to_string(),
            "340282366920938463463374607431768211456"
        );
        assert!(max < promoted);
        assert_eq!(promoted.checked_sub(&one), Some(max.clone()));
        assert_eq!(promoted.checked_sub(&max), Some(one.clone()));
        assert_eq!(max.checked_sub(&promoted), None);
        assert_eq!(promoted.checked_sub(&promoted), Some(Amount::ZERO));
        let next = promoted.checked_add(&one).unwrap();
        assert_eq!(next.checked_sub(&promoted), Some(one));
        assert_eq!(promoted.checked_sub(&next), None);
    }

    #[test]
    fn stored_addition_rejects_4097_bits() {
        let max: Amount = storage_max().to_string().parse().unwrap();
        let one = Amount::from(1);
        let just_below = max.checked_sub(&one).unwrap();
        assert_eq!(just_below.checked_add(&one), Some(max.clone()));
        assert_eq!(max.checked_add(&Amount::ZERO), Some(max.clone()));
        assert_eq!(max.checked_add(&one), None);
        assert_eq!(max.checked_add(&max), None);
        assert_eq!(Amount::ZERO.checked_sub(&one), None);
    }

    #[test]
    fn machine_prices_cross_the_u128_limit_between_78_and_79() {
        assert_eq!(Amount::machine_price(0), Some(Amount::from(20)));
        let s78 = Amount::machine_price(78).unwrap();
        let s79 = Amount::machine_price(79).unwrap();
        assert!(matches!(s78.0, Magnitude::Small(_)));
        assert!(matches!(s79.0, Magnitude::Large(_)));
        assert_eq!(s78.to_string(), "328464065365213162924629356014185105780");
        assert_eq!(s79.to_string(), "985392196095639488773888068042555317340");
    }

    #[test]
    fn machine_prices_check_the_stage_before_exponentiating() {
        let max = storage_max();
        let reference = (0..2581).fold(BigUint::from(20u8), |price, _| price * 3u8);
        assert!(reference <= max);
        assert!(&reference * 3u8 > max);
        let price = Amount::machine_price(2581).unwrap();
        assert_eq!(price.bits(), 4096);
        assert_eq!(price.to_string(), reference.to_string());
        assert_eq!(Amount::machine_price(2582), None);
        assert_eq!(Amount::machine_price(u32::MAX), None);
    }

    #[test]
    fn payment_is_level_local_and_exact() {
        let mut ledger = Ledger::default();
        ledger.set_account(
            Level::A,
            Account::new(Amount::from(8), Amount::from(4), Amount::from(2)),
        );
        ledger.set_account(
            Level::B,
            Account::new(Amount::from(100), Amount::ZERO, Amount::ZERO),
        );
        let before = ledger.clone();
        assert_eq!(
            ledger.try_pay(Level::A, &Amount::from(11)),
            Err(PaymentError::InsufficientBalance)
        );
        assert_eq!(ledger, before);
        assert_eq!(ledger.try_pay(Level::A, &Amount::from(10)), Ok(()));
        let account = ledger.account(Level::A);
        assert_eq!(account.spent(), &Amount::from(12));
        assert_eq!(account.cleared(), &Amount::from(8));
        assert_eq!(account.bonus(), &Amount::from(4));
        assert_eq!(account.available(), Balance(Magnitude::Small(0)));
        for level in [
            Level::B,
            Level::C,
            Level::D,
            Level::E,
            Level::F,
            Level::G,
            Level::H,
        ] {
            assert_eq!(ledger.account(level), before.account(level));
        }
    }

    #[test]
    fn overdrawn_account_has_zero_balance_without_changing_stored_values() {
        let mut account = Account::new(Amount::from(3), Amount::from(5), Amount::from(9));
        let before = account.clone();
        let available = account.available();
        assert_eq!(available.to_string(), "0");
        assert!(!available.covers(&Amount::from(1)));
        assert_eq!(
            account.try_pay(&Amount::from(1)),
            Err(PaymentError::InsufficientBalance)
        );
        assert_eq!(account.try_pay(&Amount::ZERO), Ok(()));
        assert_eq!(account, before);
    }

    #[test]
    fn small_payment_can_be_checked_without_adding_overflowing_small_credits() {
        let near_max = Amount::from(u128::MAX - 1);
        let mut account = Account::new(near_max.clone(), near_max.clone(), near_max);
        assert_eq!(account.try_pay(&Amount::from(1)), Ok(()));
        assert_eq!(account.spent(), &Amount::from(u128::MAX));
    }

    #[test]
    fn spending_cap_is_checked_even_if_balance_can_cover_the_charge() {
        let max: Amount = storage_max().to_string().parse().unwrap();
        let one = Amount::from(1);
        let two = Amount::from(2);
        let mut account = Account::new(max.clone(), max.clone(), max.checked_sub(&one).unwrap());
        let before = account.clone();
        assert!(account.available().covers(&two));
        assert_eq!(account.try_pay(&two), Err(PaymentError::SpendingLimit));
        assert_eq!(account, before);
        assert_eq!(account.try_pay(&one), Ok(()));
        assert_eq!(account.spent(), &max);
    }

    #[test]
    fn wide_balance_remains_exact_before_and_after_a_small_payment() {
        let reference = storage_max();
        let max: Amount = reference.to_string().parse().unwrap();
        let gross = &reference * 2u8;
        let mut account = Account::new(max.clone(), max.clone(), Amount::ZERO);
        let cost = Amount::from(20);
        assert_eq!(account.available().to_string(), gross.to_string());
        assert_eq!(account.try_pay(&cost), Ok(()));
        assert_eq!(account.spent(), &cost);
        assert_eq!(account.available().bits(), 4097);
        assert_eq!(account.available().to_string(), (gross - 20u8).to_string());
        assert_eq!(account.try_pay(&max.checked_sub(&cost).unwrap()), Ok(()));
        assert_eq!(account.spent(), &max);
        assert_eq!(account.available().to_string(), reference.to_string());
    }

    #[test]
    fn wide_intermediate_can_subtract_back_to_small_balance() {
        let max: Amount = storage_max().to_string().parse().unwrap();
        let account = Account::new(max.clone(), Amount::from(42), max);
        assert_eq!(account.available(), Balance(Magnitude::Small(42)));
    }
}
