use crate::{metrics, verification_response::VerificationResponse, verification_response::VerificationResult, verified_contract_result::Verified_Contract_Result, DB, DisplayBytes};
use actix_web::{error, web, web::Json};
use ethers_solc::EvmVersion;
use serde::Deserialize;
use smart_contract_verifier::{solidity, SolidityClient, VerificationError, Version};
use std::{collections::BTreeMap, path::PathBuf, str::FromStr};
use tracing::instrument;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct VerificationRequest {
    pub contract_address: String,
    pub creation_bytecode: Option<String>,
    pub compiler_version: String,

    #[serde(flatten)]
    pub content: MultiPartFiles,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MultiPartFiles {
    pub sources: BTreeMap<PathBuf, String>,
    pub evm_version: String,
    pub optimization_runs: Option<usize>,
    pub contract_libraries: Option<BTreeMap<String, String>>,
}

#[instrument(skip(client, params), level = "debug")]
pub async fn verify(
    client: web::Data<SolidityClient>,
    params: Json<VerificationRequest>,
) -> Result<Json<VerificationResponse>, actix_web::Error> {
    let request: smart_contract_verifier::solidity::multi_part::VerificationRequest = params.into_inner().try_into()?;
    let result = solidity::multi_part::verify(client.into_inner(), request.clone()).await;

    

    if let Ok(verification_success) = result {
        let response = VerificationResponse::ok(verification_success.into());
        metrics::count_verify_contract("solidity", &response.status, "multi-part");

        //////////////////////////////////////////////////////////////////////////////
        //////////// This is to record verification result to database ///////////////
        //////////////////////////////////////////////////////////////////////////////
        
        // Creation object of DB
        let verify_database = DB::new().await;
        // Change name of current database from DB
        let vd = verify_database.change_name("evmos");
        // Bring result of smart contract verification
        let cvr = Verified_Contract_Result {
            contract_address: request.contract_address.to_lowercase(),
            result: response.result.clone().unwrap()
        };
        // Add to database called 'evmos'
        vd.add_contract_verify_response(cvr).await;

        ///////////////////////////////////// End ////////////////////////////////////
        
        return Ok(Json(response));
    }

    let err = result.unwrap_err();
    match err {
        VerificationError::Compilation(_)
        | VerificationError::NoMatchingContracts
        | VerificationError::CompilerVersionMismatch(_) => Ok(Json(VerificationResponse::err(err))),
        VerificationError::Initialization(_) | VerificationError::VersionNotFound(_) => {
            Err(error::ErrorBadRequest(err))
        }
        VerificationError::Internal(_) => Err(error::ErrorInternalServerError(err)),
    }
}

impl TryFrom<VerificationRequest> for solidity::multi_part::VerificationRequest {
    type Error = actix_web::Error;

    fn try_from(value: VerificationRequest) -> Result<Self, Self::Error> {
        let contract_address = value.contract_address;

        let creation_bytecode = match value.creation_bytecode {
            None => None,
            Some(creation_bytecode) => Some(
                DisplayBytes::from_str(&creation_bytecode)
                    .map_err(|err| {
                        error::ErrorBadRequest(format!("Invalid creation bytecode: {err:?}"))
                    })?
                    .0,
            ),
        };
        let compiler_version = Version::from_str(&value.compiler_version)
            .map_err(|err| error::ErrorBadRequest(format!("Invalid compiler version: {err}")))?;
        Ok(Self { 
            contract_address,
            creation_bytecode,
            compiler_version,
            content: value.content.try_into()?,
        })
    }
}

impl TryFrom<MultiPartFiles> for solidity::multi_part::MultiFileContent {
    type Error = actix_web::Error;

    fn try_from(value: MultiPartFiles) -> Result<Self, Self::Error> {
        let sources: BTreeMap<PathBuf, String> = value
            .sources
            .into_iter()
            .map(|(name, content)| (name, content))
            .collect();

        let evm_version = if value.evm_version != "default" {
            Some(EvmVersion::from_str(&value.evm_version).map_err(error::ErrorBadRequest)?)
        } else {
            None
        };

        Ok(Self {
            sources,
            evm_version,
            optimization_runs: value.optimization_runs,
            contract_libraries: value.contract_libraries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::parse::test_deserialize_ok;
    use pretty_assertions::assert_eq;

    fn sources(sources: &[(&str, &str)]) -> BTreeMap<PathBuf, String> {
        sources
            .iter()
            .map(|(name, content)| (PathBuf::from(name), content.to_string()))
            .collect()
    }

    #[test]
    fn parse_multi_part() {
        test_deserialize_ok(vec![
            (
                r#"{
                        "deployed_bytecode": "0x6001",
                        "creation_bytecode": "0x6001",
                        "compiler_version": "0.8.3",
                        "sources": {
                            "source.sol": "pragma"
                        },
                        "evm_version": "london",
                        "optimization_runs": 200
                    }"#,
                VerificationRequest {
                    deployed_bytecode: "0x6001".into(),
                    creation_bytecode: Some("0x6001".into()),
                    compiler_version: "0.8.3".into(),
                    content: MultiPartFiles {
                        sources: sources(&[("source.sol", "pragma")]),
                        evm_version: format!("{}", EvmVersion::London),
                        optimization_runs: Some(200),
                        contract_libraries: None,
                    },
                },
            ),
            (
                r#"{
                    "deployed_bytecode": "0x6001",
                    "creation_bytecode": "0x6001",
                    "compiler_version": "0.8.3",
                    "sources": {
                        "source.sol": "source",
                        "A.sol": "A",
                        "B": "B",
                        "metadata.json": "metadata"
                    },
                    "evm_version": "spuriousDragon",
                    "contract_libraries": {
                        "Lib.sol": "0x1234567890123456789012345678901234567890"
                    }
                }"#,
                VerificationRequest {
                    deployed_bytecode: "0x6001".into(),
                    creation_bytecode: Some("0x6001".into()),
                    compiler_version: "0.8.3".into(),
                    content: MultiPartFiles {
                        sources: sources(&[
                            ("source.sol", "source"),
                            ("A.sol", "A"),
                            ("B", "B"),
                            ("metadata.json", "metadata"),
                        ]),
                        evm_version: format!("{}", ethers_solc::EvmVersion::SpuriousDragon),
                        optimization_runs: None,
                        contract_libraries: Some(BTreeMap::from([(
                            "Lib.sol".into(),
                            "0x1234567890123456789012345678901234567890".into(),
                        )])),
                    },
                },
            ),
        ])
    }

    #[test]
    // 'default' should result in None in MultiFileContent
    fn default_evm_version() {
        let multi_part = MultiPartFiles {
            sources: BTreeMap::new(),
            evm_version: "default".to_string(),
            optimization_runs: None,
            contract_libraries: None,
        };
        let content = solidity::multi_part::MultiFileContent::try_from(multi_part)
            .expect("Structure is valid");
        assert_eq!(
            None, content.evm_version,
            "'default' should result in `None`"
        )
    }
}
