// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use debug_ignore::DebugIgnore;
use narwhal_config::{
    Authority, Committee as ConsensusCommittee, PrimaryAddresses, Stake, WorkerAddresses,
};
use rand::rngs::OsRng;
use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
};
use sui_types::{base_types::encode_bytes_hex, crypto::get_key_pair_from_rng};

use crate::{
    genesis, new_network_address, utils, CommitteeConfig, ConsensusConfig, GenesisConfig,
    NetworkConfig, NodeConfig, ValidatorGenesisInfo, ValidatorInfo, AUTHORITIES_DB_NAME,
    CONSENSUS_DB_NAME, DEFAULT_STAKE,
};

pub struct ConfigBuilder<R = OsRng> {
    rng: R,
    config_directory: PathBuf,
    randomize_ports: bool,
    committee_size: NonZeroUsize,
    initial_accounts_config: Option<GenesisConfig>,
}

impl ConfigBuilder {
    pub fn new<P: AsRef<Path>>(config_directory: P) -> Self {
        Self {
            rng: OsRng,
            config_directory: config_directory.as_ref().into(),
            randomize_ports: true,
            committee_size: NonZeroUsize::new(1).unwrap(),
            initial_accounts_config: None,
        }
    }
}

impl<R> ConfigBuilder<R> {
    pub fn randomize_ports(mut self, randomize_ports: bool) -> Self {
        self.randomize_ports = randomize_ports;
        self
    }

    pub fn committee_size(mut self, committee_size: NonZeroUsize) -> Self {
        self.committee_size = committee_size;
        self
    }

    pub fn initial_accounts_config(mut self, initial_accounts_config: GenesisConfig) -> Self {
        self.initial_accounts_config = Some(initial_accounts_config);
        self
    }

    pub fn rng<N: ::rand::RngCore + ::rand::CryptoRng>(self, rng: N) -> ConfigBuilder<N> {
        ConfigBuilder {
            rng,
            config_directory: self.config_directory,
            randomize_ports: self.randomize_ports,
            committee_size: self.committee_size,
            initial_accounts_config: self.initial_accounts_config,
        }
    }
}

impl<R: ::rand::RngCore + ::rand::CryptoRng> ConfigBuilder<R> {
    //TODO right now we always randomize ports, we may want to have a default port configuration
    pub fn build(mut self) -> NetworkConfig {
        let validators = (0..self.committee_size.get())
            .map(|_| get_key_pair_from_rng(&mut self.rng).1)
            .map(|key_pair| ValidatorGenesisInfo {
                key_pair,
                network_address: new_network_address(),
                stake: DEFAULT_STAKE,
                narwhal_primary_to_primary: new_network_address(),
                narwhal_worker_to_primary: new_network_address(),
                narwhal_primary_to_worker: new_network_address(),
                narwhal_worker_to_worker: new_network_address(),
                narwhal_consensus_address: new_network_address(),
            })
            .collect::<Vec<_>>();

        self.build_with_validators(validators)
    }

    pub fn build_with_validators(mut self, validators: Vec<ValidatorGenesisInfo>) -> NetworkConfig {
        let epoch = 0;

        let validator_set = validators
            .iter()
            .map(|validator| {
                let public_key = *validator.key_pair.public_key_bytes();
                let stake = validator.stake;
                let network_address = validator.network_address.clone();

                ValidatorInfo {
                    public_key,
                    stake,
                    network_address,
                }
            })
            .collect::<Vec<_>>();

        let initial_accounts_config = self
            .initial_accounts_config
            .unwrap_or_else(GenesisConfig::for_local_testing);
        let (account_keys, objects) = initial_accounts_config
            .generate_accounts(&mut self.rng)
            .unwrap();
        // It is important that we create a single genesis ctx, and use it to generate
        // modules and objects from now on. This ensures all object IDs created are unique.
        let mut genesis_ctx = sui_adapter::genesis::get_genesis_context();
        let custom_modules = initial_accounts_config
            .generate_custom_move_modules(&mut genesis_ctx)
            .unwrap();

        let genesis = {
            let mut builder = genesis::Builder::new(genesis_ctx)
                .add_move_modules(custom_modules)
                .add_objects(objects);

            for validator in &validator_set {
                builder = builder.add_validator(validator.public_key(), validator.stake());
            }

            builder.build()
        };

        let narwhal_committee = validators
            .iter()
            .map(|validator| {
                let name = validator
                    .key_pair
                    .public_key_bytes()
                    .make_narwhal_public_key()
                    .expect("Can't get narwhal public key");
                let primary = PrimaryAddresses {
                    primary_to_primary: validator.narwhal_primary_to_primary.clone(),
                    worker_to_primary: validator.narwhal_worker_to_primary.clone(),
                };
                let workers = [(
                    0, // worker_id
                    WorkerAddresses {
                        primary_to_worker: validator.narwhal_primary_to_worker.clone(),
                        transactions: validator.narwhal_consensus_address.clone(),
                        worker_to_worker: validator.narwhal_worker_to_worker.clone(),
                    },
                )]
                .into_iter()
                .collect();
                let authority = Authority {
                    stake: validator.stake as Stake, //TODO this should at least be the same size integer
                    primary,
                    workers,
                };

                (name, authority)
            })
            .collect();
        let consensus_committee = ConsensusCommittee {
            authorities: narwhal_committee,
        };

        let committee_config = CommitteeConfig {
            epoch,
            validator_set,
            consensus_committee: DebugIgnore(consensus_committee),
        };

        let validator_configs = validators
            .into_iter()
            .map(|validator| {
                let public_key = validator.key_pair.public_key_bytes();
                let db_path = self
                    .config_directory
                    .join(AUTHORITIES_DB_NAME)
                    .join(encode_bytes_hex(public_key));
                let network_address = validator.network_address;
                let consensus_address = validator.narwhal_consensus_address;
                let consensus_db_path = self
                    .config_directory
                    .join(CONSENSUS_DB_NAME)
                    .join(encode_bytes_hex(public_key));
                let consensus_config = ConsensusConfig {
                    consensus_address,
                    consensus_db_path,
                    narwhal_config: Default::default(),
                };

                let metrics_address = new_network_address();

                NodeConfig {
                    key_pair: validator.key_pair,
                    db_path,
                    network_address,
                    metrics_address,
                    json_rpc_address: format!("127.0.0.1:{}", utils::get_available_port())
                        .parse()
                        .unwrap(),
                    consensus_config: Some(consensus_config),
                    committee_config: committee_config.clone(),
                    genesis: genesis.clone(),
                }
            })
            .collect();

        NetworkConfig {
            validator_configs,
            loaded_move_packages: vec![],
            genesis,
            account_keys,
        }
    }
}
