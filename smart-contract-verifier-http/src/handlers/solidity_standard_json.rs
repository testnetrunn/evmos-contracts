use crate::{metrics, verification_response::VerificationResponse, verification_response::VerificationResult, verified_contract_result::Verified_Contract_Result, DB, DisplayBytes};
use actix_web::{error, web, web::Json};
use anyhow::anyhow;
use ethers_solc::CompilerInput;
use serde::Deserialize;
use smart_contract_verifier::{solidity, SolidityClient, VerificationError, Version};
use std::str::FromStr;
use thiserror::Error;
use tracing::instrument;

#[derive(Debug, Clone, Deserialize)]
pub struct VerificationRequest {
    pub contract_address: String,
    pub creation_bytecode: Option<String>,
    pub compiler_version: String,

    #[serde(flatten)]
    pub content: StandardJson,
}

#[instrument(skip(client, params), level = "debug")]
pub async fn verify(
    client: web::Data<SolidityClient>,
    params: Json<VerificationRequest>,
) -> Result<Json<VerificationResponse>, actix_web::Error> {
    let request: smart_contract_verifier::solidity::standard_json::VerificationRequest = {
        let request: Result<_, ParseError> = params.into_inner().try_into();
        if let Err(err) = request {
            match err {
                ParseError::InvalidContent(_) => return Err(error::ErrorBadRequest(err)),
                ParseError::BadRequest(_) => return Ok(Json(VerificationResponse::err(err))),
            }
        }
        request.unwrap()
    };

    let result = solidity::standard_json::verify(client.into_inner(), request.clone()).await;

    if let Ok(verification_success) = result {
        let response = VerificationResponse::ok(verification_success.into());
        metrics::count_verify_contract("solidity", &response.status, "json");

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

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("content is not valid standard json: {0}")]
    InvalidContent(#[from] serde_json::Error),
    #[error("{0}")]
    BadRequest(#[from] anyhow::Error),
}

#[derive(Clone, Debug, Deserialize)]
pub struct StandardJson {
    input: String,
}

impl TryFrom<VerificationRequest> for solidity::standard_json::VerificationRequest {
    type Error = ParseError;

    fn try_from(value: VerificationRequest) -> Result<Self, Self::Error> {
        let contract_address = value.contract_address;
        let creation_bytecode = match value.creation_bytecode {
            None => None,
            Some(creation_bytecode) => Some(
                DisplayBytes::from_str(&creation_bytecode)
                    .map_err(|err| anyhow!("Invalid creation bytecode: {:?}", err))?
                    .0,
            ),
        };
        let compiler_version = Version::from_str(&value.compiler_version)
            .map_err(|err| anyhow!("Invalid compiler version: {}", err))?;
        Ok(Self {
            contract_address,
            creation_bytecode,
            compiler_version,
            content: value.content.try_into()?,
        })
    }
}

impl TryFrom<StandardJson> for solidity::standard_json::StandardJsonContent {
    type Error = ParseError;

    fn try_from(value: StandardJson) -> Result<Self, Self::Error> {
        let input: CompilerInput = serde_json::from_str(&value.input)?;

        Ok(Self { input })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_standard_json() {
        let input = r#"{
            "deployed_bytecode": "0x6001",
            "creation_bytecode": "0x6001",
            "compiler_version": "v0.8.2+commit.661d1103",
            "input": "{\"language\": \"Solidity\", \"sources\": {\"./src/contracts/Foo.sol\": {\"content\": \"pragma solidity ^0.8.2;\\n\\ncontract Foo {\\n    function bar() external pure returns (uint256) {\\n        return 42;\\n    }\\n}\\n\"}}, \"settings\": {\"metadata\": {\"useLiteralContent\": true}, \"optimizer\": {\"enabled\": true, \"runs\": 200}, \"outputSelection\": {\"*\": {\"*\": [\"abi\", \"evm.bytecode\", \"evm.deployedBytecode\", \"evm.methodIdentifiers\"], \"\": [\"id\", \"ast\"]}}}}"
        }"#;

        let deserialized: VerificationRequest = serde_json::from_str(input).expect("Valid json");
        assert_eq!(
            deserialized.deployed_bytecode, "0x6001",
            "Invalid deployed bytecode"
        );
        assert_eq!(
            deserialized.creation_bytecode,
            Some("0x6001".into()),
            "Invalid creation bytecode"
        );
        assert_eq!(
            deserialized.compiler_version, "v0.8.2+commit.661d1103",
            "Invalid compiler version"
        );
        let _compiler_input: solidity::standard_json::StandardJsonContent = deserialized
            .content
            .try_into()
            .expect("failed to convert to standard json");
    }
}
