//! Optional pubkeys that can be used a `Pod`s
#[cfg(feature = "borsh")]
use borsh::{BorshDeserialize, BorshSchema, BorshSerialize};
#[cfg(feature = "serde-traits")]
use {
    base64::{prelude::BASE64_STANDARD, Engine},
    serde::de::{Error, Unexpected, Visitor},
    serde::{Deserialize, Deserializer, Serialize, Serializer},
    std::{convert::TryFrom, fmt, str::FromStr},
};
use {
    bytemuck::{Pod, Zeroable},
    solana_program::{program_error::ProgramError, program_option::COption, pubkey::Pubkey},
    solana_zk_token_sdk::zk_token_elgamal::pod::ElGamalPubkey,
};

/// A Pubkey that encodes `None` as all `0`, meant to be usable as a Pod type,
/// similar to all NonZero* number types from the bytemuck library.
#[cfg_attr(
    feature = "borsh",
    derive(BorshDeserialize, BorshSerialize, BorshSchema)
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(transparent)]
pub struct OptionalNonZeroPubkey(Pubkey);
impl TryFrom<Option<Pubkey>> for OptionalNonZeroPubkey {
    type Error = ProgramError;
    fn try_from(p: Option<Pubkey>) -> Result<Self, Self::Error> {
        match p {
            None => Ok(Self(Pubkey::default())),
            Some(pubkey) => {
                if pubkey == Pubkey::default() {
                    Err(ProgramError::InvalidArgument)
                } else {
                    Ok(Self(pubkey))
                }
            }
        }
    }
}
impl TryFrom<COption<Pubkey>> for OptionalNonZeroPubkey {
    type Error = ProgramError;
    fn try_from(p: COption<Pubkey>) -> Result<Self, Self::Error> {
        match p {
            COption::None => Ok(Self(Pubkey::default())),
            COption::Some(pubkey) => {
                if pubkey == Pubkey::default() {
                    Err(ProgramError::InvalidArgument)
                } else {
                    Ok(Self(pubkey))
                }
            }
        }
    }
}
impl From<OptionalNonZeroPubkey> for Option<Pubkey> {
    fn from(p: OptionalNonZeroPubkey) -> Self {
        if p.0 == Pubkey::default() {
            None
        } else {
            Some(p.0)
        }
    }
}
impl From<OptionalNonZeroPubkey> for COption<Pubkey> {
    fn from(p: OptionalNonZeroPubkey) -> Self {
        if p.0 == Pubkey::default() {
            COption::None
        } else {
            COption::Some(p.0)
        }
    }
}

#[cfg(feature = "serde-traits")]
impl Serialize for OptionalNonZeroPubkey {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.0 == Pubkey::default() {
            s.serialize_none()
        } else {
            s.serialize_some(&self.0.to_string())
        }
    }
}

#[cfg(feature = "serde-traits")]
/// Visitor for deserializing OptionalNonZeroPubkey
struct OptionalNonZeroPubkeyVisitor;

#[cfg(feature = "serde-traits")]
impl<'de> Visitor<'de> for OptionalNonZeroPubkeyVisitor {
    type Value = OptionalNonZeroPubkey;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a Pubkey in base58 or `null`")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        let pkey = Pubkey::from_str(&v)
            .map_err(|_| Error::invalid_value(Unexpected::Str(v), &"value string"))?;

        OptionalNonZeroPubkey::try_from(Some(pkey))
            .map_err(|_| Error::custom("Failed to convert from pubkey"))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: Error,
    {
        OptionalNonZeroPubkey::try_from(None).map_err(|e| Error::custom(e.to_string()))
    }
}

#[cfg(feature = "serde-traits")]
impl<'de> Deserialize<'de> for OptionalNonZeroPubkey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(OptionalNonZeroPubkeyVisitor)
    }
}

/// An ElGamalPubkey that encodes `None` as all `0`, meant to be usable as a Pod
/// type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(transparent)]
pub struct OptionalNonZeroElGamalPubkey(ElGamalPubkey);
impl OptionalNonZeroElGamalPubkey {
    /// Checks equality between an OptionalNonZeroElGamalPubkey and an
    /// ElGamalPubkey when interpreted as bytes.
    pub fn equals(&self, other: &ElGamalPubkey) -> bool {
        &self.0 == other
    }
}
impl TryFrom<Option<ElGamalPubkey>> for OptionalNonZeroElGamalPubkey {
    type Error = ProgramError;
    fn try_from(p: Option<ElGamalPubkey>) -> Result<Self, Self::Error> {
        match p {
            None => Ok(Self(ElGamalPubkey::default())),
            Some(elgamal_pubkey) => {
                if elgamal_pubkey == ElGamalPubkey::default() {
                    Err(ProgramError::InvalidArgument)
                } else {
                    Ok(Self(elgamal_pubkey))
                }
            }
        }
    }
}
impl From<OptionalNonZeroElGamalPubkey> for Option<ElGamalPubkey> {
    fn from(p: OptionalNonZeroElGamalPubkey) -> Self {
        if p.0 == ElGamalPubkey::default() {
            None
        } else {
            Some(p.0)
        }
    }
}

#[cfg(any(feature = "serde-traits", test))]
const OPTIONAL_NONZERO_ELGAMAL_PUBKEY_LEN: usize = 32;

#[cfg(feature = "serde-traits")]
impl Serialize for OptionalNonZeroElGamalPubkey {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.0 == ElGamalPubkey::default() {
            s.serialize_none()
        } else {
            s.serialize_some(&self.0.to_string())
        }
    }
}

#[cfg(feature = "serde-traits")]
struct OptionalNonZeroElGamalPubkeyVisitor;

#[cfg(feature = "serde-traits")]
impl<'de> Visitor<'de> for OptionalNonZeroElGamalPubkeyVisitor {
    type Value = OptionalNonZeroElGamalPubkey;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("an ElGamal public key as base64 or `null`")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        let bytes = BASE64_STANDARD.decode(v).map_err(Error::custom)?;

        if bytes.len() != OPTIONAL_NONZERO_ELGAMAL_PUBKEY_LEN {
            return Err(Error::custom(format!(
                "Length of base64 decoded bytes is not {}",
                OPTIONAL_NONZERO_ELGAMAL_PUBKEY_LEN
            )));
        }

        let mut array = [0; OPTIONAL_NONZERO_ELGAMAL_PUBKEY_LEN];
        array.copy_from_slice(&bytes[0..OPTIONAL_NONZERO_ELGAMAL_PUBKEY_LEN]);
        let elgamal_pubkey = ElGamalPubkey(array);
        OptionalNonZeroElGamalPubkey::try_from(Some(elgamal_pubkey)).map_err(Error::custom)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(OptionalNonZeroElGamalPubkey::default())
    }
}

#[cfg(feature = "serde-traits")]
impl<'de> Deserialize<'de> for OptionalNonZeroElGamalPubkey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(OptionalNonZeroElGamalPubkeyVisitor)
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::bytemuck::pod_from_bytes, solana_program::pubkey::PUBKEY_BYTES};

    #[test]
    fn test_pod_non_zero_option() {
        assert_eq!(
            Some(Pubkey::new_from_array([1; PUBKEY_BYTES])),
            Option::<Pubkey>::from(
                *pod_from_bytes::<OptionalNonZeroPubkey>(&[1; PUBKEY_BYTES]).unwrap()
            )
        );
        assert_eq!(
            None,
            Option::<Pubkey>::from(
                *pod_from_bytes::<OptionalNonZeroPubkey>(&[0; PUBKEY_BYTES]).unwrap()
            )
        );
        assert_eq!(
            pod_from_bytes::<OptionalNonZeroPubkey>(&[]).unwrap_err(),
            ProgramError::InvalidArgument
        );
        assert_eq!(
            pod_from_bytes::<OptionalNonZeroPubkey>(&[0; 1]).unwrap_err(),
            ProgramError::InvalidArgument
        );
        assert_eq!(
            pod_from_bytes::<OptionalNonZeroPubkey>(&[1; 1]).unwrap_err(),
            ProgramError::InvalidArgument
        );
    }

    #[cfg(feature = "serde-traits")]
    #[test]
    fn test_pod_non_zero_option_serde_some() {
        let optional_non_zero_pubkey_some =
            OptionalNonZeroPubkey(Pubkey::new_from_array([1; PUBKEY_BYTES]));
        let serialized_some = serde_json::to_string(&optional_non_zero_pubkey_some).unwrap();
        assert_eq!(
            &serialized_some,
            "\"4vJ9JU1bJJE96FWSJKvHsmmFADCg4gpZQff4P3bkLKi\""
        );

        let deserialized_some =
            serde_json::from_str::<OptionalNonZeroPubkey>(&serialized_some).unwrap();
        assert_eq!(optional_non_zero_pubkey_some, deserialized_some);
    }

    #[cfg(feature = "serde-traits")]
    #[test]
    fn test_pod_non_zero_option_serde_none() {
        let optional_non_zero_pubkey_none =
            OptionalNonZeroPubkey(Pubkey::new_from_array([0; PUBKEY_BYTES]));
        let serialized_none = serde_json::to_string(&optional_non_zero_pubkey_none).unwrap();
        assert_eq!(&serialized_none, "null");

        let deserialized_none =
            serde_json::from_str::<OptionalNonZeroPubkey>(&serialized_none).unwrap();
        assert_eq!(optional_non_zero_pubkey_none, deserialized_none);
    }

    #[test]
    fn test_pod_non_zero_elgamal_option() {
        assert_eq!(
            Some(ElGamalPubkey([1; OPTIONAL_NONZERO_ELGAMAL_PUBKEY_LEN])),
            Option::<ElGamalPubkey>::from(OptionalNonZeroElGamalPubkey(ElGamalPubkey(
                [1; OPTIONAL_NONZERO_ELGAMAL_PUBKEY_LEN]
            )))
        );
        assert_eq!(
            None,
            Option::<ElGamalPubkey>::from(OptionalNonZeroElGamalPubkey(ElGamalPubkey(
                [0; OPTIONAL_NONZERO_ELGAMAL_PUBKEY_LEN]
            )))
        );

        assert_eq!(
            OptionalNonZeroElGamalPubkey(ElGamalPubkey([1; OPTIONAL_NONZERO_ELGAMAL_PUBKEY_LEN])),
            *pod_from_bytes::<OptionalNonZeroElGamalPubkey>(
                &[1; OPTIONAL_NONZERO_ELGAMAL_PUBKEY_LEN]
            )
            .unwrap()
        );
        assert!(pod_from_bytes::<OptionalNonZeroElGamalPubkey>(&[]).is_err());
    }

    #[cfg(feature = "serde-traits")]
    #[test]
    fn test_pod_non_zero_elgamal_option_serde_some() {
        let optional_non_zero_elgamal_pubkey_some =
            OptionalNonZeroElGamalPubkey(ElGamalPubkey([1; OPTIONAL_NONZERO_ELGAMAL_PUBKEY_LEN]));
        let serialized_some =
            serde_json::to_string(&optional_non_zero_elgamal_pubkey_some).unwrap();
        assert_eq!(
            &serialized_some,
            "\"AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=\""
        );

        let deserialized_some =
            serde_json::from_str::<OptionalNonZeroElGamalPubkey>(&serialized_some).unwrap();
        assert_eq!(optional_non_zero_elgamal_pubkey_some, deserialized_some);
    }

    #[cfg(feature = "serde-traits")]
    #[test]
    fn test_pod_non_zero_elgamal_option_serde_none() {
        let optional_non_zero_elgamal_pubkey_none =
            OptionalNonZeroElGamalPubkey(ElGamalPubkey([0; OPTIONAL_NONZERO_ELGAMAL_PUBKEY_LEN]));
        let serialized_none =
            serde_json::to_string(&optional_non_zero_elgamal_pubkey_none).unwrap();
        assert_eq!(&serialized_none, "null");

        let deserialized_none =
            serde_json::from_str::<OptionalNonZeroElGamalPubkey>(&serialized_none).unwrap();
        assert_eq!(optional_non_zero_elgamal_pubkey_none, deserialized_none);
    }
}
