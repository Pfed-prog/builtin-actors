use std::collections::HashMap;

use frc46_token::token::state::decode_actor_id;
use fvm_ipld_blockstore::Blockstore;
use fvm_shared::ActorID;
use fvm_shared::address::{Address, Protocol};
use fvm_shared::clock::ChainEpoch;
use num_traits::Signed;

use fil_actors_runtime::runtime::policy_constants::{
    MAXIMUM_VERIFIED_ALLOCATION_EXPIRATION, MAXIMUM_VERIFIED_ALLOCATION_TERM,
    MINIMUM_VERIFIED_ALLOCATION_SIZE, MINIMUM_VERIFIED_ALLOCATION_TERM,
};
use fil_actors_runtime::{DEFAULT_HAMT_CONFIG, Map2, MessageAccumulator};

use crate::{Allocation, AllocationID, Claim, ClaimID, DataCap, State};

pub struct StateSummary {
    pub verifiers: HashMap<Address, DataCap>,
    pub allocations: HashMap<AllocationID, Allocation>,
    pub claims: HashMap<ClaimID, Claim>,
}

/// Checks internal invariants of verified registry state.
pub fn check_state_invariants<BS: Blockstore>(
    state: &State,
    store: BS,
    prior_epoch: ChainEpoch,
) -> (StateSummary, MessageAccumulator) {
    let acc: MessageAccumulator = MessageAccumulator::default();

    // Load and check verifiers
    let mut all_verifiers: HashMap<Address, fvm_shared::bigint::BigInt> = HashMap::new();
    match state.load_verifiers(&store) {
        Ok(verifiers) => {
            let ret: Result<_, _> = verifiers.for_each(|verifier: Address, cap| {
                acc.require(
                    verifier.protocol() == Protocol::ID,
                    format!("verifier {verifier} should have ID protocol"),
                );
                acc.require(
                    !cap.0.is_negative(),
                    format!("verifier {verifier} cap {} is negative", cap.0),
                );
                all_verifiers.insert(verifier, cap.clone().0);
                Ok(())
            });

            acc.require_no_error(ret, "error iterating verifiers");
        }
        Err(e) => acc.add(format!("error loading verifiers {e}")),
    }

    // Load and check allocations
    let mut all_allocations: HashMap<u64, _> = HashMap::new();
    match state.load_allocs(&store) {
        Ok(allocations) => {
            let ret: Result<_, _> = allocations.for_each(|client_key, inner_root| {
                let client_id: u64 = decode_actor_id(client_key).unwrap();
                let inner: Result<_, _> = Map2::<&BS, AllocationID, Allocation>::load(
                    &store,
                    inner_root,
                    DEFAULT_HAMT_CONFIG,
                    "allocations inner",
                );
                match inner {
                    Ok(allocations) => {
                        let ret: Result<(), fil_actors_runtime::ActorError> = Ok(allocations.for_each(|allocation_id: u64, allocation: &Allocation| {
                            check_allocation_state(
                                allocation_id,
                                allocation,
                                client_id,
                                state.next_allocation_id,
                                prior_epoch,
                                &acc,
                            );

                            all_allocations.insert(allocation_id, allocation.clone());
                            Ok(())
                        }).expect("allocations"));
                        acc.require_no_error(
                            ret,
                            format!("error iterating allocations inner for {client_id}"),
                        );
                    }
                    Err(e) => acc.add(format!("error loading allocations {e}")),
                }
                Ok(())
            });

            acc.require_no_error(ret, "error iterating allocations outer");
        }
        Err(e) => acc.add(format!("error loading allocations from {e}")),
    }

    let mut all_claims: HashMap<u64, _> = HashMap::new();
    match state.load_claims(&store) {
        Ok(claims) => {
            let ret: Result<_, _> = claims.for_each(|provider_key: &fvm_ipld_hamt::BytesKey, inner_root| {
                let provider_id: u64 = decode_actor_id(provider_key).unwrap();
                let inner: Result<_, _> = Map2::<&BS, ClaimID, Claim>::load(
                    &store,
                    inner_root,
                    DEFAULT_HAMT_CONFIG,
                    "allocations inner",
                );
                match inner {
                    Ok(claims) => {
                        let ret: Result<(), fil_actors_runtime::ActorError> = Ok(claims.for_each(|claim_id: u64, claim: &Claim| {
                            check_claim_state(
                                claim_id,
                                claim,
                                provider_id,
                                state.next_allocation_id,
                                prior_epoch,
                                &acc,
                            );
                            all_claims.insert(claim_id, claim.clone());
                            Ok(())
                        }).expect("claims"));
                        acc.require_no_error(
                            ret,
                            format!("error iterating allocations inner for {provider_id}"),
                        );
                    }
                    Err(e) => acc.add(format!("error loading allocations {e}")),
                }
                Ok(())
            });

            acc.require_no_error(ret, "error iterating allocations outer");
        }
        Err(e) => acc.add(format!("error loading claims {e}")),
    }

    (
        StateSummary { verifiers: all_verifiers, allocations: all_allocations, claims: all_claims },
        acc,
    )
}

fn check_allocation_state(
    id: u64,
    alloc: &Allocation,
    client: ActorID,
    next_alloc_id: u64,
    prior_epoch: ChainEpoch,
    acc: &MessageAccumulator,
) {
    acc.require(id < next_alloc_id, format!("allocation id {} exceeds next {}", id, next_alloc_id));
    acc.require(
        alloc.client == client,
        format!("allocation {} client {} doesn't match key {}", id, alloc.client, client),
    );
    acc.require(
        alloc.size.0 >= MINIMUM_VERIFIED_ALLOCATION_SIZE as u64,
        format!("allocation {} size {} too small", id, alloc.size.0),
    );
    acc.require(
        alloc.term_min >= MINIMUM_VERIFIED_ALLOCATION_TERM,
        format!("allocation {} term min {} too small", id, alloc.term_min),
    );
    acc.require(
        alloc.term_max <= MAXIMUM_VERIFIED_ALLOCATION_TERM,
        format!("allocation {} term max {} too large ", id, alloc.term_max),
    );
    acc.require(
        alloc.term_min <= alloc.term_max,
        format!("allocation {} term min {} exceeds max {}", id, alloc.term_min, alloc.term_max),
    );
    acc.require(
        alloc.expiration <= prior_epoch + MAXIMUM_VERIFIED_ALLOCATION_EXPIRATION,
        format!(
            "allocation {} expiration {} too far from now {}",
            id, alloc.expiration, prior_epoch
        ),
    )
}

fn check_claim_state(
    id: u64,
    claim: &Claim,
    provider: ActorID,
    next_alloc_id: u64,
    prior_epoch: ChainEpoch,
    acc: &MessageAccumulator,
) {
    acc.require(id < next_alloc_id, format!("claim id {} exceeds next {}", id, next_alloc_id));
    acc.require(
        claim.provider == provider,
        format!("claim {} provider {} doesn't match key {}", id, claim.provider, provider),
    );
    acc.require(
        claim.size.0 >= MINIMUM_VERIFIED_ALLOCATION_SIZE as u64,
        format!(
            "claim {} size {} below minimum {}",
            id, claim.size.0, MINIMUM_VERIFIED_ALLOCATION_SIZE
        ),
    );
    acc.require(
        claim.term_min >= MINIMUM_VERIFIED_ALLOCATION_TERM,
        format!(
            "claim {} term min {} below minimum {}",
            id, claim.term_min, MINIMUM_VERIFIED_ALLOCATION_TERM
        ),
    );
    // The maximum term is not limited because it can be extended
    // arbitrarily long by a client spending new datacap.
    acc.require(
        claim.term_min <= claim.term_max,
        format!("claim {} term min {} exceeds max {}", id, claim.term_min, claim.term_max),
    );
    acc.require(
        claim.term_start <= prior_epoch,
        format!("claim {} term start {} after now {}", id, claim.term_start, prior_epoch),
    );
}
