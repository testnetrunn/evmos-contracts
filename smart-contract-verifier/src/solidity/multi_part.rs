use super::client::Client;
use crate::{
    compiler::Version,
    verifier::{ContractVerifier, Error, Success},
};
use bytes::Bytes;
use ethers_solc::{
    artifacts::{BytecodeHash, Libraries, Settings, SettingsMetadata, Source, Sources},
    CompilerInput, EvmVersion,
};
use semver::VersionReq;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use web3_rpc::web3::Web3;
use actix_web::error;
use std::str::FromStr;
use crate::DisplayBytes;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationRequest {
    pub contract_address: String,
    pub creation_bytecode: Option<Bytes>,
    pub compiler_version: Version,

    pub content: MultiFileContent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiFileContent {
    pub sources: BTreeMap<PathBuf, String>,
    pub evm_version: Option<EvmVersion>,
    pub optimization_runs: Option<usize>,
    pub contract_libraries: Option<BTreeMap<String, String>>,
}

impl From<MultiFileContent> for Vec<CompilerInput> {
    fn from(content: MultiFileContent) -> Self {
        let mut settings = Settings::default();
        settings.optimizer.enabled = Some(content.optimization_runs.is_some());
        settings.optimizer.runs = content.optimization_runs;
        if let Some(libs) = content.contract_libraries {
            // we have to know filename for library, but we don't know,
            // so we assume that every file MAY contains all libraries
            let libs = content
                .sources
                .keys()
                .map(|filename| (PathBuf::from(filename), libs.clone()))
                .collect();
            settings.libraries = Libraries { libs };
        }
        settings.evm_version = content.evm_version;

        let sources: Sources = content
            .sources
            .into_iter()
            .map(|(name, content)| (name, Source { content }))
            .collect();
        let inputs: Vec<_> = CompilerInput::with_sources(sources)
            .into_iter()
            .map(|input| input.settings(settings.clone()))
            .collect();
        inputs
    }
}

pub async fn get_Code(contract_address: &str) -> Result<Option<String>, anyhow::Error> {
    let rpc = Web3::new("https://evmos-evm.publicnode.com".to_string());
    match rpc.eth_get_code(contract_address, None).await {
        Ok(r) =>  {println!("Fetching success!"); return Ok(r.result)},
        Err(e) => {
            tracing::error!("There is no contract {}", e);
            Err(e)
        }
    }
}

pub async fn verify(client: Arc<Client>, request: VerificationRequest) -> Result<Success, Error> {
    let compiler_version = request.compiler_version;

    let _deployed_bytecode = get_Code(request.contract_address.as_str()).await.expect("invalid address address.");
    
    let deployed_bytecode = DisplayBytes::from_str(_deployed_bytecode.expect("no deployed bytecode for this address.").as_str()).expect("invalide bytecode").0;
    println!("deployed bytecode is {:?}", deployed_bytecode);
    let verifier = ContractVerifier::new(
                client.compilers(),
                &compiler_version,
                request.creation_bytecode,
                deployed_bytecode
            )?;
    // println!("in solidity::multi_part::verify: {:?}", get_Code(request.contract_address.as_str()).await);
    // let deployed_bytecode = DisplayBytes::from_str(&value.deployed_bytecode)
    //         .map_err(|err| error::ErrorBadRequest(format!("Invalid deployed bytecode: {err:?}")))?
    //         .0;

    let compiler_inputs: Vec<CompilerInput> = request.content.into();
    for mut compiler_input in compiler_inputs {
        for metadata in settings_metadata(&compiler_version) {
            compiler_input.settings.metadata = metadata;
            let result = verifier.verify(&compiler_input).await;

            // If no matching contracts have been found, try the next settings metadata option
            if let Err(Error::NoMatchingContracts) = result {
                continue;
            }

            // If any error, it is uncorrectable and should be returned immediately, otherwise
            // we allow middlewares to process success and only then return it to the caller
            let success = result?;
            if let Some(middleware) = client.middleware() {
                middleware.call(&success).await;
            }
            return Ok(success);
        }
    }

    // No contracts could be verified
    Err(Error::NoMatchingContracts)
}

/// Iterates through possible bytecode if required and creates
/// a corresponding variants of settings metadata for each of them.
///
/// Multi-file input type does not specify it explicitly, thus, we may
/// have to iterate through all possible options.
///
/// See "settings_metadata" (https://docs.soliditylang.org/en/v0.8.15/using-the-compiler.html?highlight=compiler%20input#input-description)
fn settings_metadata(compiler_version: &Version) -> Vec<Option<SettingsMetadata>> {
    // Options are sorted by their probability of occurring
    const BYTECODE_HASHES: [BytecodeHash; 3] =
        [BytecodeHash::Ipfs, BytecodeHash::None, BytecodeHash::Bzzr1];

    if VersionReq::parse("<0.6.0")
        .unwrap()
        .matches(compiler_version.version())
    {
        [None].into()
    } else {
        BYTECODE_HASHES
            .map(|hash| Some(SettingsMetadata::from(hash)))
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn sources(sources: &[(&str, &str)]) -> BTreeMap<PathBuf, String> {
        sources
            .iter()
            .map(|(name, content)| (PathBuf::from(name), content.to_string()))
            .collect()
    }

    fn test_to_input(multi_part: MultiFileContent, expected: Vec<&str>) {
        let inputs: Vec<CompilerInput> = multi_part.try_into().unwrap();
        assert_eq!(
            inputs.len(),
            expected.len(),
            "invalid number of compiler inputs"
        );
        for i in 0..expected.len() {
            let input_json = serde_json::to_string(&inputs[i]).unwrap();
            println!("{input_json}");
            assert_eq!(input_json, expected[i]);
        }
    }

    #[test]
    fn multi_part_to_input() {
        let multi_part = MultiFileContent {
            sources: sources(&[("source.sol", "pragma")]),
            evm_version: Some(EvmVersion::London),
            optimization_runs: Some(200),
            contract_libraries: Some(BTreeMap::from([(
                "some_library".into(),
                "some_address".into(),
            )])),
        };
        let expected = r#"{"language":"Solidity","sources":{"source.sol":{"content":"pragma"}},"settings":{"optimizer":{"enabled":true,"runs":200},"outputSelection":{"*":{"":["ast"],"*":["abi","evm.bytecode","evm.deployedBytecode","evm.methodIdentifiers"]}},"evmVersion":"london","libraries":{"source.sol":{"some_library":"some_address"}}}}"#;
        test_to_input(multi_part, vec![expected]);
        let multi_part = MultiFileContent {
            sources: sources(&[("source.sol", "")]),
            evm_version: Some(EvmVersion::SpuriousDragon),
            optimization_runs: None,
            contract_libraries: None,
        };
        let expected = r#"{"language":"Solidity","sources":{"source.sol":{"content":""}},"settings":{"optimizer":{"enabled":false},"outputSelection":{"*":{"":["ast"],"*":["abi","evm.bytecode","evm.deployedBytecode","evm.methodIdentifiers"]}},"evmVersion":"spuriousDragon","libraries":{}}}"#;
        test_to_input(multi_part, vec![expected]);
    }

    #[test]
    fn yul_and_solidity_to_inputs() {
        let multi_part = MultiFileContent {
            sources: sources(&[("source.sol", "pragma"), ("source2.yul", "object \"A\" {}")]),
            evm_version: Some(EvmVersion::London),
            optimization_runs: Some(200),
            contract_libraries: None,
        };
        let expected_solidity = r#"{"language":"Solidity","sources":{"source.sol":{"content":"pragma"}},"settings":{"optimizer":{"enabled":true,"runs":200},"outputSelection":{"*":{"":["ast"],"*":["abi","evm.bytecode","evm.deployedBytecode","evm.methodIdentifiers"]}},"evmVersion":"london","libraries":{}}}"#;
        let expected_yul = r#"{"language":"Yul","sources":{"source2.yul":{"content":"object \"A\" {}"}},"settings":{"optimizer":{"enabled":true,"runs":200},"outputSelection":{"*":{"":["ast"],"*":["abi","evm.bytecode","evm.deployedBytecode","evm.methodIdentifiers"]}},"evmVersion":"london","libraries":{}}}"#;
        test_to_input(multi_part, vec![expected_solidity, expected_yul]);
    }
}
