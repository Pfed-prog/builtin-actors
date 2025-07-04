use std::cell::RefCell;
use std::collections::HashMap;

use cid::Cid;

use frc46_token::receiver::{FRC46_TOKEN_TYPE, FRC46TokenReceived};
use frc46_token::token::TOKEN_PRECISION;
use frc46_token::token::types::{BurnParams, BurnReturn, TransferParams};
use fvm_actor_utils::receiver::UniversalReceiverParams;
use fvm_ipld_encoding::RawBytes;
use fvm_ipld_encoding::ipld_block::IpldBlock;
use fvm_shared::address::Address;
use fvm_shared::bigint::bigint_ser::BigIntSer;
use fvm_shared::clock::ChainEpoch;
use fvm_shared::econ::TokenAmount;
use fvm_shared::error::ExitCode;
use fvm_shared::piece::PaddedPieceSize;
use fvm_shared::sector::SectorNumber;
use fvm_shared::sys::SendFlags;
use fvm_shared::{ActorID, MethodNum};
use num_traits::{ToPrimitive, Zero};

use fil_actor_verifreg::state::{DATACAP_MAP_CONFIG, DataCapMap};
use fil_actor_verifreg::testing::check_state_invariants;
use fil_actor_verifreg::{
    Actor as VerifregActor, AddVerifiedClientParams, AddVerifierParams, Allocation,
    AllocationClaim, AllocationID, AllocationRequest, AllocationRequests, AllocationsResponse,
    Claim, ClaimAllocationsParams, ClaimAllocationsReturn, ClaimExtensionRequest, ClaimID, DataCap,
    ExtendClaimTermsParams, ExtendClaimTermsReturn, GetClaimsParams, GetClaimsReturn, Method,
    RemoveExpiredAllocationsParams, RemoveExpiredAllocationsReturn, RemoveExpiredClaimsParams,
    RemoveExpiredClaimsReturn, SectorAllocationClaims, State, ext,
};
use fil_actors_runtime::cbor::serialize;
use fil_actors_runtime::runtime::Runtime;
use fil_actors_runtime::runtime::builtins::Type;
use fil_actors_runtime::runtime::policy_constants::{
    MAXIMUM_VERIFIED_ALLOCATION_TERM, MINIMUM_VERIFIED_ALLOCATION_TERM,
};
use fil_actors_runtime::test_utils::*;
use fil_actors_runtime::{
    ActorError, AsActorError, BatchReturn, DATACAP_TOKEN_ACTOR_ADDR, EventBuilder,
    STORAGE_MARKET_ACTOR_ADDR, SYSTEM_ACTOR_ADDR, VERIFIED_REGISTRY_ACTOR_ADDR,
};

pub const ROOT_ADDR: Address = Address::new_id(101);

const TEST_VERIFIER_ADDR: u64 = 201;
const TEST_VERIFIER2_ADDR: u64 = 202;
const TEST_CLIENT_ADDR: u64 = 301;
const TEST_CLIENT2_ADDR: u64 = 302;
const TEST_CLIENT3_ADDR: u64 = 303;
const TEST_CLIENT4_ADDR: u64 = 304;

pub fn new_runtime() -> MockRuntime {
    let test_verifier_addr = Address::new_id(TEST_VERIFIER_ADDR);
    let test_verifier2_addr = Address::new_id(TEST_VERIFIER2_ADDR);
    let test_client_addr = Address::new_id(TEST_CLIENT_ADDR);
    let test_client2_addr = Address::new_id(TEST_CLIENT2_ADDR);
    let test_client3_addr = Address::new_id(TEST_CLIENT3_ADDR);
    let test_client4_addr = Address::new_id(TEST_CLIENT4_ADDR);
    let mut actor_code_cids = HashMap::default();
    actor_code_cids.insert(test_verifier_addr, *ACCOUNT_ACTOR_CODE_ID);
    actor_code_cids.insert(test_verifier2_addr, *ACCOUNT_ACTOR_CODE_ID);
    actor_code_cids.insert(test_client_addr, *ACCOUNT_ACTOR_CODE_ID);
    actor_code_cids.insert(test_client2_addr, *ACCOUNT_ACTOR_CODE_ID);
    actor_code_cids.insert(test_client3_addr, *ACCOUNT_ACTOR_CODE_ID);
    actor_code_cids.insert(test_client4_addr, *ACCOUNT_ACTOR_CODE_ID);
    MockRuntime {
        receiver: VERIFIED_REGISTRY_ACTOR_ADDR,
        caller: RefCell::new(SYSTEM_ACTOR_ADDR),
        caller_type: RefCell::new(*SYSTEM_ACTOR_CODE_ID),
        actor_code_cids: RefCell::new(actor_code_cids),
        ..Default::default()
    }
}

// Sets the miner code/type for an actor ID
pub fn add_miner(rt: &MockRuntime, id: ActorID) {
    rt.set_address_actor_type(Address::new_id(id), *MINER_ACTOR_CODE_ID);
}

pub fn new_harness() -> (Harness, MockRuntime) {
    let rt = new_runtime();
    let h = Harness { root: ROOT_ADDR };
    h.construct_and_verify(&rt, &h.root);
    (h, rt)
}

pub struct Harness {
    pub root: Address,
}

impl Harness {
    pub fn construct_and_verify(&self, rt: &MockRuntime, root_param: &Address) {
        rt.set_caller(*SYSTEM_ACTOR_CODE_ID, SYSTEM_ACTOR_ADDR);
        rt.expect_validate_caller_addr(vec![SYSTEM_ACTOR_ADDR]);
        let ret = rt
            .call::<VerifregActor>(
                Method::Constructor as MethodNum,
                IpldBlock::serialize_cbor(root_param).unwrap(),
            )
            .unwrap();

        assert!(ret.is_none());
        rt.verify();

        let empty_map = DataCapMap::empty(&rt.store, DATACAP_MAP_CONFIG, "empty").flush().unwrap();
        let state: State = rt.get_state();
        assert_eq!(self.root, state.root_key);
        assert_eq!(empty_map, state.verifiers);
    }

    pub fn add_verifier(
        &self,
        rt: &MockRuntime,
        verifier: &Address,
        allowance: &DataCap,
    ) -> Result<(), ActorError> {
        self.add_verifier_with_existing_cap(rt, verifier, allowance, &DataCap::zero())
    }

    pub fn add_verifier_with_existing_cap(
        &self,
        rt: &MockRuntime,
        verifier: &Address,
        allowance: &DataCap,
        cap: &DataCap, // Mocked data cap balance of the prospective verifier
    ) -> Result<(), ActorError> {
        rt.expect_validate_caller_addr(vec![self.root]);
        rt.set_caller(*ACCOUNT_ACTOR_CODE_ID, self.root);
        let verifier_resolved = rt.get_id_address(verifier).unwrap_or(*verifier);
        // Expect checking the verifier's token balance.
        rt.expect_send(
            DATACAP_TOKEN_ACTOR_ADDR,
            ext::datacap::Method::Balance as MethodNum,
            IpldBlock::serialize_cbor(&verifier_resolved).unwrap(),
            TokenAmount::zero(),
            None,
            SendFlags::READ_ONLY,
            IpldBlock::serialize_cbor(&BigIntSer(&(cap * TOKEN_PRECISION))).unwrap(),
            ExitCode::OK,
            None,
        );

        rt.expect_emitted_event(
            EventBuilder::new()
                .typ("verifier-balance")
                .field_indexed("verifier", &verifier_resolved.id().unwrap())
                .field("balance", &BigIntSer(allowance))
                .build()?,
        );

        let params = AddVerifierParams { address: *verifier, allowance: allowance.clone() };
        let ret = rt.call::<VerifregActor>(
            Method::AddVerifier as MethodNum,
            IpldBlock::serialize_cbor(&params).unwrap(),
        )?;
        assert!(ret.is_none());
        rt.verify();

        self.assert_verifier_allowance(rt, verifier, allowance);
        Ok(())
    }

    pub fn remove_verifier(&self, rt: &MockRuntime, verifier: &Address) -> Result<(), ActorError> {
        rt.expect_validate_caller_addr(vec![self.root]);

        rt.expect_emitted_event(
            EventBuilder::new()
                .typ("verifier-balance")
                .field_indexed("verifier", &verifier.id().unwrap())
                .field("balance", &BigIntSer(&DataCap::zero()))
                .build()?,
        );
        rt.set_caller(*ACCOUNT_ACTOR_CODE_ID, self.root);
        let ret = rt.call::<VerifregActor>(
            Method::RemoveVerifier as MethodNum,
            IpldBlock::serialize_cbor(verifier).unwrap(),
        )?;
        assert!(ret.is_none());
        rt.verify();

        self.assert_verifier_removed(rt, verifier);
        Ok(())
    }

    pub fn assert_verifier_allowance(
        &self,
        rt: &MockRuntime,
        verifier: &Address,
        allowance: &DataCap,
    ) {
        let verifier_id_addr = rt.get_id_address(verifier).unwrap();
        assert_eq!(*allowance, self.get_verifier_allowance(rt, &verifier_id_addr));
    }

    pub fn get_verifier_allowance(&self, rt: &MockRuntime, verifier: &Address) -> DataCap {
        let verifiers = rt.get_state::<State>().load_verifiers(&rt.store).unwrap();
        verifiers.get(verifier).unwrap().unwrap().clone().0
    }

    pub fn assert_verifier_removed(&self, rt: &MockRuntime, verifier: &Address) {
        let verifier_id_addr = rt.get_id_address(verifier).unwrap();
        let verifiers = rt.get_state::<State>().load_verifiers(&rt.store).unwrap();
        assert!(!verifiers.contains_key(&verifier_id_addr).unwrap())
    }

    pub fn add_client(
        &self,
        rt: &MockRuntime,
        verifier: &Address,
        client: &Address,
        allowance: &DataCap,
        verifier_balance: &DataCap,
    ) -> Result<(), ActorError> {
        rt.expect_validate_caller_any();
        rt.set_caller(*ACCOUNT_ACTOR_CODE_ID, *verifier);
        let client_resolved = rt.get_id_address(client).unwrap_or(*client);

        // Expect tokens to be minted.
        let mint_params = ext::datacap::MintParams {
            to: client_resolved,
            amount: TokenAmount::from_whole(allowance.to_i64().unwrap()),
            operators: vec![STORAGE_MARKET_ACTOR_ADDR],
        };
        rt.expect_send_simple(
            DATACAP_TOKEN_ACTOR_ADDR,
            ext::datacap::Method::Mint as MethodNum,
            IpldBlock::serialize_cbor(&mint_params).unwrap(),
            TokenAmount::zero(),
            None,
            ExitCode::OK,
        );

        let params = AddVerifiedClientParams { address: *client, allowance: allowance.clone() };
        if client_resolved.id().is_ok() {
            // if the client isn't resolved, we don't expect an event because the call should abort
            rt.expect_emitted_event(
                EventBuilder::new()
                    .typ("verifier-balance")
                    .field_indexed("verifier", &verifier.id().unwrap())
                    .field("balance", &BigIntSer(&(verifier_balance - allowance)))
                    .field_indexed("client", &client_resolved.id().unwrap())
                    .build()?,
            );
        }
        let ret = rt.call::<VerifregActor>(
            Method::AddVerifiedClient as MethodNum,
            IpldBlock::serialize_cbor(&params).unwrap(),
        )?;
        assert!(ret.is_none());
        rt.verify();

        Ok(())
    }

    pub fn check_state(&self, rt: &MockRuntime) {
        let (_, acc) = check_state_invariants(&rt.get_state(), rt.store(), *rt.epoch.borrow());
        acc.assert_empty();
    }

    // TODO this should be implemented through a call to verifreg but for now it modifies state directly
    pub fn create_alloc(
        &self,
        rt: &MockRuntime,
        alloc: &Allocation,
    ) -> Result<AllocationID, ActorError> {
        let mut st: State = rt.get_state();
        let mut allocs = st.load_allocs(rt.store()).unwrap();
        let alloc_id = st.next_allocation_id;
        assert!(
            allocs
                .put_if_absent(alloc.client, alloc_id, alloc.clone())
                .context_code(ExitCode::USR_ILLEGAL_STATE, "faild to put")?
        );
        st.next_allocation_id += 1;
        st.allocations = allocs.flush().expect("failed flushing allocation table");
        rt.replace_state(&st);
        Ok(alloc_id)
    }

    pub fn load_alloc(
        &self,
        rt: &MockRuntime,
        client: ActorID,
        id: AllocationID,
    ) -> Option<Allocation> {
        let st: State = rt.get_state();
        let mut allocs = st.load_allocs(rt.store()).unwrap();
        allocs.get(client, id).unwrap().cloned()
    }

    // Invokes the ClaimAllocations actor method
    pub fn claim_allocations(
        &self,
        rt: &MockRuntime,
        provider: ActorID,
        claim_allocs: Vec<SectorAllocationClaims>,
        datacap_burnt: u64,
        all_or_nothing: bool,
        expect_claimed: Vec<(AllocationID, Allocation, SectorNumber)>,
    ) -> Result<ClaimAllocationsReturn, ActorError> {
        rt.expect_validate_caller_type(vec![Type::Miner]);
        rt.set_caller(*MINER_ACTOR_CODE_ID, Address::new_id(provider));

        for (id, alloc, sector) in expect_claimed.iter() {
            expect_claim_emitted(
                rt,
                "claim",
                *id,
                alloc.client,
                alloc.provider,
                &alloc.data,
                alloc.size.0,
                *sector,
                alloc.term_min,
                alloc.term_max,
                0,
            )
        }

        if datacap_burnt > 0 {
            rt.expect_send_simple(
                DATACAP_TOKEN_ACTOR_ADDR,
                ext::datacap::Method::Burn as MethodNum,
                IpldBlock::serialize_cbor(&BurnParams {
                    amount: TokenAmount::from_whole(datacap_burnt.to_i64().unwrap()),
                })
                .unwrap(),
                TokenAmount::zero(),
                IpldBlock::serialize_cbor(&BurnReturn { balance: TokenAmount::zero() }).unwrap(),
                ExitCode::OK,
            );
        }

        let params = ClaimAllocationsParams { sectors: claim_allocs, all_or_nothing };
        let ret = rt
            .call::<VerifregActor>(
                Method::ClaimAllocations as MethodNum,
                IpldBlock::serialize_cbor(&params).unwrap(),
            )?
            .unwrap()
            .deserialize()
            .expect("failed to deserialize claim allocations return");
        rt.verify();
        Ok(ret)
    }

    // Invokes the RemoveExpiredAllocations actor method.
    pub fn remove_expired_allocations(
        &self,
        rt: &MockRuntime,
        client: ActorID,
        allocation_ids: Vec<AllocationID>,
        expect_removed: Vec<(AllocationID, Allocation)>,
    ) -> Result<RemoveExpiredAllocationsReturn, ActorError> {
        rt.expect_validate_caller_any();

        let mut expected_datacap = 0u64;
        for (id, alloc) in expect_removed {
            expected_datacap += alloc.size.0;
            expect_allocation_emitted(
                rt,
                "allocation-removed",
                id,
                alloc.client,
                alloc.provider,
                &alloc.data,
                alloc.size.0,
                alloc.term_min,
                alloc.term_max,
                alloc.expiration,
            )
        }
        rt.expect_send_simple(
            DATACAP_TOKEN_ACTOR_ADDR,
            ext::datacap::Method::Transfer as MethodNum,
            IpldBlock::serialize_cbor(&TransferParams {
                to: Address::new_id(client),
                amount: TokenAmount::from_whole(expected_datacap.to_i64().unwrap()),
                operator_data: RawBytes::default(),
            })
            .unwrap(),
            TokenAmount::zero(),
            None,
            ExitCode::OK,
        );

        let params = RemoveExpiredAllocationsParams { client, allocation_ids };
        let ret = rt
            .call::<VerifregActor>(
                Method::RemoveExpiredAllocations as MethodNum,
                IpldBlock::serialize_cbor(&params).unwrap(),
            )?
            .unwrap()
            .deserialize()
            .expect("failed to deserialize remove expired allocations return");
        rt.verify();
        Ok(ret)
    }

    // Invokes the RemoveExpiredClaims actor method.
    pub fn remove_expired_claims(
        &self,
        rt: &MockRuntime,
        provider: ActorID,
        claim_ids: Vec<ClaimID>,
        expect_removed: Vec<(ClaimID, Claim)>,
    ) -> Result<RemoveExpiredClaimsReturn, ActorError> {
        rt.expect_validate_caller_any();

        for (id, claim) in expect_removed {
            expect_claim_emitted(
                rt,
                "claim-removed",
                id,
                claim.client,
                claim.provider,
                &claim.data,
                claim.size.0,
                claim.sector,
                claim.term_min,
                claim.term_max,
                claim.term_start,
            )
        }

        let params = RemoveExpiredClaimsParams { provider, claim_ids };
        let ret = rt
            .call::<VerifregActor>(
                Method::RemoveExpiredClaims as MethodNum,
                IpldBlock::serialize_cbor(&params).unwrap(),
            )?
            .unwrap()
            .deserialize()
            .expect("failed to deserialize remove expired claims return");
        rt.verify();
        Ok(ret)
    }

    pub fn load_claim(&self, rt: &MockRuntime, provider: ActorID, id: ClaimID) -> Option<Claim> {
        let st: State = rt.get_state();
        let mut claims = st.load_claims(rt.store()).unwrap();
        claims.get(provider, id).unwrap().cloned()
    }

    pub fn receive_tokens(
        &self,
        rt: &MockRuntime,
        payload: FRC46TokenReceived,
        expected_alloc_results: BatchReturn,
        expected_extension_results: BatchReturn,
        expected_alloc_ids: Vec<AllocationID>,
        expected_burn: u64,
    ) -> Result<(), ActorError> {
        rt.set_caller(*DATACAP_TOKEN_ACTOR_CODE_ID, DATACAP_TOKEN_ACTOR_ADDR);
        let params = UniversalReceiverParams {
            type_: FRC46_TOKEN_TYPE,
            payload: serialize(&payload, "payload").unwrap(),
        };

        if !expected_burn.is_zero() {
            rt.expect_send_simple(
                DATACAP_TOKEN_ACTOR_ADDR,
                ext::datacap::Method::Burn as MethodNum,
                IpldBlock::serialize_cbor(&BurnParams {
                    amount: TokenAmount::from_whole(expected_burn),
                })
                .unwrap(),
                TokenAmount::zero(),
                IpldBlock::serialize_cbor(&BurnReturn { balance: TokenAmount::zero() }).unwrap(),
                ExitCode::OK,
            );
        }

        let allocs_req: AllocationRequests = payload.operator_data.deserialize().unwrap();
        for (alloc, id) in allocs_req.allocations.iter().zip(expected_alloc_ids.iter()) {
            expect_allocation_emitted(
                rt,
                "allocation",
                *id,
                payload.from,
                alloc.provider,
                &alloc.data,
                alloc.size.0,
                alloc.term_min,
                alloc.term_max,
                alloc.expiration,
            )
        }

        for ext in allocs_req.extensions {
            let mut claim = self.load_claim(rt, ext.provider, ext.claim).unwrap();
            claim.term_max = ext.term_max;
            expect_claim_emitted(
                rt,
                "claim-updated",
                ext.claim,
                claim.client,
                claim.provider,
                &claim.data,
                claim.size.0,
                claim.sector,
                claim.term_min,
                claim.term_max,
                claim.term_start,
            )
        }

        rt.expect_validate_caller_addr(vec![DATACAP_TOKEN_ACTOR_ADDR]);
        let ret = rt.call::<VerifregActor>(
            Method::UniversalReceiverHook as MethodNum,
            IpldBlock::serialize_cbor(&params).unwrap(),
        )?;
        assert_eq!(
            AllocationsResponse {
                allocation_results: expected_alloc_results,
                extension_results: expected_extension_results,
                new_allocations: expected_alloc_ids,
            },
            ret.unwrap().deserialize().unwrap()
        );
        rt.verify();
        Ok(())
    }

    // Creates a claim directly in state.
    pub fn create_claim(&self, rt: &MockRuntime, claim: &Claim) -> Result<ClaimID, ActorError> {
        let mut st: State = rt.get_state();
        let mut claims = st.load_claims(rt.store()).unwrap();
        let id = st.next_allocation_id;
        assert!(
            claims
                .put_if_absent(claim.provider, id, claim.clone())
                .context_code(ExitCode::USR_ILLEGAL_STATE, "faild to put")?
        );
        st.next_allocation_id += 1;
        st.claims = claims.flush().expect("failed flushing allocation table");
        rt.replace_state(&st);
        Ok(id)
    }

    pub fn get_claims(
        &self,
        rt: &MockRuntime,
        provider: ActorID,
        claim_ids: Vec<ClaimID>,
    ) -> Result<GetClaimsReturn, ActorError> {
        rt.expect_validate_caller_any();
        let params = GetClaimsParams { claim_ids, provider };
        let ret = rt
            .call::<VerifregActor>(
                Method::GetClaims as MethodNum,
                IpldBlock::serialize_cbor(&params).unwrap(),
            )?
            .unwrap()
            .deserialize()
            .expect("failed to deserialize get claims return");
        rt.verify();
        Ok(ret)
    }

    pub fn extend_claim_terms(
        &self,
        rt: &MockRuntime,
        params: &ExtendClaimTermsParams,
        expected: Vec<(ClaimID, Claim)>,
    ) -> Result<ExtendClaimTermsReturn, ActorError> {
        for (id, mut new_claim) in expected {
            let ext = params.terms.iter().find(|c| c.claim_id == id).unwrap();
            new_claim.term_max = ext.term_max;
            expect_claim_emitted(
                rt,
                "claim-updated",
                id,
                new_claim.client,
                new_claim.provider,
                &new_claim.data,
                new_claim.size.0,
                new_claim.sector,
                new_claim.term_min,
                new_claim.term_max,
                new_claim.term_start,
            )
        }

        rt.expect_validate_caller_any();
        let ret = rt
            .call::<VerifregActor>(
                Method::ExtendClaimTerms as MethodNum,
                IpldBlock::serialize_cbor(&params).unwrap(),
            )?
            .unwrap()
            .deserialize()
            .expect("failed to deserialize extend claim terms return");
        rt.verify();
        Ok(ret)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn expect_allocation_emitted(
    rt: &MockRuntime,
    typ: &str,
    id: u64,
    client: ActorID,
    provider: ActorID,
    piece_cid: &Cid,
    piece_size: u64,
    term_min: ChainEpoch,
    term_max: ChainEpoch,
    expiration: ChainEpoch,
) {
    rt.expect_emitted_event(
        EventBuilder::new()
            .typ(typ)
            .field_indexed("id", &id)
            .field_indexed("client", &client)
            .field_indexed("provider", &provider)
            .field_indexed("piece-cid", piece_cid)
            .field("piece-size", &piece_size)
            .field("term-min", &term_min)
            .field("term-max", &term_max)
            .field("expiration", &expiration)
            .build()
            .unwrap(),
    );
}

#[allow(clippy::too_many_arguments)]
pub fn expect_claim_emitted(
    rt: &MockRuntime,
    typ: &str,
    id: u64,
    client: ActorID,
    provider: ActorID,
    piece_cid: &Cid,
    piece_size: u64,
    sector: SectorNumber,
    term_min: ChainEpoch,
    term_max: ChainEpoch,
    term_start: ChainEpoch,
) {
    rt.expect_emitted_event(
        EventBuilder::new()
            .typ(typ)
            .field_indexed("id", &id)
            .field_indexed("client", &client)
            .field_indexed("provider", &provider)
            .field_indexed("piece-cid", piece_cid)
            .field("piece-size", &piece_size)
            .field("term-min", &term_min)
            .field("term-max", &term_max)
            .field("term-start", &term_start)
            .field_indexed("sector", &sector)
            .build()
            .unwrap(),
    );
}

pub fn make_alloc(data_id: &str, client: ActorID, provider: ActorID, size: u64) -> Allocation {
    Allocation {
        client,
        provider,
        data: make_piece_cid(data_id.as_bytes()),
        size: PaddedPieceSize(size),
        term_min: MINIMUM_VERIFIED_ALLOCATION_TERM,
        term_max: MINIMUM_VERIFIED_ALLOCATION_TERM * 2,
        expiration: 100,
    }
}

// Creates an allocation request for fixed data with default terms.
pub fn make_alloc_req(rt: &MockRuntime, provider: ActorID, size: u64) -> AllocationRequest {
    AllocationRequest {
        provider,
        data: make_piece_cid("1234".as_bytes()),
        size: PaddedPieceSize(size),
        term_min: MINIMUM_VERIFIED_ALLOCATION_TERM,
        term_max: MAXIMUM_VERIFIED_ALLOCATION_TERM,
        expiration: *rt.epoch.borrow() + 100,
    }
}

pub fn make_extension_req(
    provider: ActorID,
    claim: ClaimID,
    term_max: ChainEpoch,
) -> ClaimExtensionRequest {
    ClaimExtensionRequest { provider, claim, term_max }
}

// Creates the expected allocation from a request.
pub fn alloc_from_req(client: ActorID, req: &AllocationRequest) -> Allocation {
    Allocation {
        client,
        provider: req.provider,
        data: req.data,
        size: req.size,
        term_min: req.term_min,
        term_max: req.term_max,
        expiration: req.expiration,
    }
}

pub fn make_claim_reqs(
    sector: SectorNumber,
    expiry: ChainEpoch,
    allocs: &[(AllocationID, &Allocation)],
) -> SectorAllocationClaims {
    SectorAllocationClaims {
        sector,
        expiry,
        claims: allocs
            .iter()
            .map(|(id, alloc)| AllocationClaim {
                client: alloc.client,
                allocation_id: *id,
                data: alloc.data,
                size: alloc.size,
            })
            .collect(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn make_claim(
    data_id: &str,
    client: ActorID,
    provider: ActorID,
    size: u64,
    term_min: i64,
    term_max: i64,
    term_start: i64,
    sector: u64,
) -> Claim {
    Claim {
        provider,
        client,
        data: make_piece_cid(data_id.as_bytes()),
        size: PaddedPieceSize(size),
        term_min,
        term_max,
        term_start,
        sector,
    }
}

pub fn claim_from_alloc(alloc: &Allocation, term_start: ChainEpoch, sector: SectorNumber) -> Claim {
    Claim {
        provider: alloc.provider,
        client: alloc.client,
        data: alloc.data,
        size: alloc.size,
        term_min: alloc.term_min,
        term_max: alloc.term_max,
        term_start,
        sector,
    }
}

pub fn make_receiver_hook_token_payload(
    client: ActorID,
    alloc_requests: Vec<AllocationRequest>,
    extension_requests: Vec<ClaimExtensionRequest>,
    datacap_received: u64,
) -> FRC46TokenReceived {
    // let total_size: u64 = alloc_requests.iter().map(|r| r.size.0).sum();
    let payload =
        AllocationRequests { allocations: alloc_requests, extensions: extension_requests };
    FRC46TokenReceived {
        from: client,
        to: VERIFIED_REGISTRY_ACTOR_ADDR.id().unwrap(),
        operator: client,
        amount: TokenAmount::from_whole(datacap_received as i64),
        operator_data: serialize(&payload, "operator data").unwrap(),
        token_data: Default::default(),
    }
}

pub fn assert_allocation(
    rt: &MockRuntime,
    client: ActorID,
    id: AllocationID,
    expected: &Allocation,
) {
    let st: State = rt.get_state();
    let store = &rt.store();
    let mut allocs = st.load_allocs(store).unwrap();

    assert_eq!(expected, allocs.get(client, id).unwrap().unwrap());
}

pub fn assert_claim(rt: &MockRuntime, provider: ActorID, id: ClaimID, expected: &Claim) {
    let st: State = rt.get_state();
    let store = &rt.store();
    let mut claims = st.load_claims(store).unwrap();

    assert_eq!(expected, claims.get(provider, id).unwrap().unwrap());
}

pub fn assert_alloc_claimed(
    rt: &MockRuntime,
    client: ActorID,
    provider: ActorID,
    id: ClaimID,
    alloc: &Allocation,
    epoch: ChainEpoch,
    sector: SectorNumber,
) -> Claim {
    let st: State = rt.get_state();
    let store = &rt.store();

    // Alloc is gone
    let mut allocs = st.load_allocs(&store).unwrap();
    assert!(allocs.get(client, id).unwrap().is_none());

    // Claim is present
    let expected_claim = claim_from_alloc(alloc, epoch, sector);
    assert_eq!(client, expected_claim.client); // Check the caller provided sensible arguments.
    assert_eq!(provider, expected_claim.provider);
    let mut claims = st.load_claims(store).unwrap();
    assert_eq!(&expected_claim, claims.get(provider, id).unwrap().unwrap());
    expected_claim
}
