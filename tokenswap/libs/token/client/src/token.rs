use {
    crate::{
        client::{ProgramClient, ProgramClientError, SendTransaction, SimulateTransaction},
        proof_generation::transfer_with_fee_split_proof_data,
    },
    futures::{future::join_all, try_join},
    futures_util::TryFutureExt,
    solana_program_test::tokio::time,
    solana_sdk::{
        account::Account as BaseAccount,
        hash::Hash,
        instruction::{AccountMeta, Instruction},
        message::Message,
        program_error::ProgramError,
        program_pack::Pack,
        pubkey::Pubkey,
        signer::{signers::Signers, Signer, SignerError},
        system_instruction,
        transaction::Transaction,
    },
    spl_associated_token_account::{
        get_associated_token_address_with_program_id,
        instruction::{
            create_associated_token_account, create_associated_token_account_idempotent,
        },
    },
    spl_token_2022::{
        extension::{
            confidential_transfer::{
                self,
                account_info::{
                    ApplyPendingBalanceAccountInfo, EmptyAccountAccountInfo, TransferAccountInfo,
                    WithdrawAccountInfo,
                },
                ciphertext_extraction::SourceDecryptHandles,
                instruction::{
                    TransferSplitContextStateAccounts, TransferWithFeeSplitContextStateAccounts,
                },
                ConfidentialTransferAccount, DecryptableBalance,
            },
            confidential_transfer_fee::{
                self, account_info::WithheldTokensInfo, ConfidentialTransferFeeAmount,
                ConfidentialTransferFeeConfig,
            },
            cpi_guard, default_account_state, group_member_pointer, group_pointer,
            interest_bearing_mint, memo_transfer, metadata_pointer, transfer_fee, transfer_hook,
            BaseStateWithExtensions, Extension, ExtensionType, StateWithExtensionsOwned,
        },
        instruction, offchain,
        proof::ProofLocation,
        solana_zk_token_sdk::{
            encryption::{
                auth_encryption::AeKey,
                elgamal::{ElGamalCiphertext, ElGamalKeypair, ElGamalPubkey, ElGamalSecretKey},
            },
            instruction::*,
            zk_token_elgamal::pod::ElGamalPubkey as PodElGamalPubkey,
            zk_token_proof_instruction::{self, ContextStateInfo, ProofInstruction},
            zk_token_proof_program,
            zk_token_proof_state::ProofContextState,
        },
        state::{Account, AccountState, Mint, Multisig},
    },
    spl_token_group_interface::state::{TokenGroup, TokenGroupMember},
    spl_token_metadata_interface::state::{Field, TokenMetadata},
    std::{
        fmt, io,
        mem::size_of,
        sync::{Arc, RwLock},
        time::{Duration, Instant},
    },
    thiserror::Error,
};

#[derive(Error, Debug)]
pub enum TokenError {
    #[error("client error: {0}")]
    Client(ProgramClientError),
    #[error("program error: {0}")]
    Program(#[from] ProgramError),
    #[error("account not found")]
    AccountNotFound,
    #[error("invalid account owner")]
    AccountInvalidOwner,
    #[error("invalid account mint")]
    AccountInvalidMint,
    #[error("invalid associated account address")]
    AccountInvalidAssociatedAddress,
    #[error("invalid auxiliary account address")]
    AccountInvalidAuxiliaryAddress,
    #[error("proof generation")]
    ProofGeneration,
    #[error("maximum deposit transfer amount exceeded")]
    MaximumDepositTransferAmountExceeded,
    #[error("encryption key error")]
    Key(SignerError),
    #[error("account decryption failed")]
    AccountDecryption,
    #[error("not enough funds in account")]
    NotEnoughFunds,
    #[error("missing memo signer")]
    MissingMemoSigner,
    #[error("decimals required, but missing")]
    MissingDecimals,
    #[error("decimals specified, but incorrect")]
    InvalidDecimals,
}
impl PartialEq for TokenError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            // TODO not great, but workable for tests
            // currently missing: proof error, signer error
            (Self::Client(ref a), Self::Client(ref b)) => a.to_string() == b.to_string(),
            (Self::Program(ref a), Self::Program(ref b)) => a == b,
            (Self::AccountNotFound, Self::AccountNotFound) => true,
            (Self::AccountInvalidOwner, Self::AccountInvalidOwner) => true,
            (Self::AccountInvalidMint, Self::AccountInvalidMint) => true,
            (Self::AccountInvalidAssociatedAddress, Self::AccountInvalidAssociatedAddress) => true,
            (Self::AccountInvalidAuxiliaryAddress, Self::AccountInvalidAuxiliaryAddress) => true,
            (Self::ProofGeneration, Self::ProofGeneration) => true,
            (
                Self::MaximumDepositTransferAmountExceeded,
                Self::MaximumDepositTransferAmountExceeded,
            ) => true,
            (Self::AccountDecryption, Self::AccountDecryption) => true,
            (Self::NotEnoughFunds, Self::NotEnoughFunds) => true,
            (Self::MissingMemoSigner, Self::MissingMemoSigner) => true,
            (Self::MissingDecimals, Self::MissingDecimals) => true,
            (Self::InvalidDecimals, Self::InvalidDecimals) => true,
            _ => false,
        }
    }
}

/// Encapsulates initializing an extension
#[derive(Clone, Debug, PartialEq)]
pub enum ExtensionInitializationParams {
    ConfidentialTransferMint {
        authority: Option<Pubkey>,
        auto_approve_new_accounts: bool,
        auditor_elgamal_pubkey: Option<PodElGamalPubkey>,
    },
    DefaultAccountState {
        state: AccountState,
    },
    MintCloseAuthority {
        close_authority: Option<Pubkey>,
    },
    TransferFeeConfig {
        transfer_fee_config_authority: Option<Pubkey>,
        withdraw_withheld_authority: Option<Pubkey>,
        transfer_fee_basis_points: u16,
        maximum_fee: u64,
    },
    InterestBearingConfig {
        rate_authority: Option<Pubkey>,
        rate: i16,
    },
    NonTransferable,
    PermanentDelegate {
        delegate: Pubkey,
    },
    TransferHook {
        authority: Option<Pubkey>,
        program_id: Option<Pubkey>,
    },
    MetadataPointer {
        authority: Option<Pubkey>,
        metadata_address: Option<Pubkey>,
    },
    ConfidentialTransferFeeConfig {
        authority: Option<Pubkey>,
        withdraw_withheld_authority_elgamal_pubkey: PodElGamalPubkey,
    },
    GroupPointer {
        authority: Option<Pubkey>,
        group_address: Option<Pubkey>,
    },
    GroupMemberPointer {
        authority: Option<Pubkey>,
        member_address: Option<Pubkey>,
    },
}
impl ExtensionInitializationParams {
    /// Get the extension type associated with the init params
    pub fn extension(&self) -> ExtensionType {
        match self {
            Self::ConfidentialTransferMint { .. } => ExtensionType::ConfidentialTransferMint,
            Self::DefaultAccountState { .. } => ExtensionType::DefaultAccountState,
            Self::MintCloseAuthority { .. } => ExtensionType::MintCloseAuthority,
            Self::TransferFeeConfig { .. } => ExtensionType::TransferFeeConfig,
            Self::InterestBearingConfig { .. } => ExtensionType::InterestBearingConfig,
            Self::NonTransferable => ExtensionType::NonTransferable,
            Self::PermanentDelegate { .. } => ExtensionType::PermanentDelegate,
            Self::TransferHook { .. } => ExtensionType::TransferHook,
            Self::MetadataPointer { .. } => ExtensionType::MetadataPointer,
            Self::ConfidentialTransferFeeConfig { .. } => {
                ExtensionType::ConfidentialTransferFeeConfig
            }
            Self::GroupPointer { .. } => ExtensionType::GroupPointer,
            Self::GroupMemberPointer { .. } => ExtensionType::GroupMemberPointer,
        }
    }
    /// Generate an appropriate initialization instruction for the given mint
    pub fn instruction(
        self,
        token_program_id: &Pubkey,
        mint: &Pubkey,
    ) -> Result<Instruction, ProgramError> {
        match self {
            Self::ConfidentialTransferMint {
                authority,
                auto_approve_new_accounts,
                auditor_elgamal_pubkey,
            } => confidential_transfer::instruction::initialize_mint(
                token_program_id,
                mint,
                authority,
                auto_approve_new_accounts,
                auditor_elgamal_pubkey,
            ),
            Self::DefaultAccountState { state } => {
                default_account_state::instruction::initialize_default_account_state(
                    token_program_id,
                    mint,
                    &state,
                )
            }
            Self::MintCloseAuthority { close_authority } => {
                instruction::initialize_mint_close_authority(
                    token_program_id,
                    mint,
                    close_authority.as_ref(),
                )
            }
            Self::TransferFeeConfig {
                transfer_fee_config_authority,
                withdraw_withheld_authority,
                transfer_fee_basis_points,
                maximum_fee,
            } => transfer_fee::instruction::initialize_transfer_fee_config(
                token_program_id,
                mint,
                transfer_fee_config_authority.as_ref(),
                withdraw_withheld_authority.as_ref(),
                transfer_fee_basis_points,
                maximum_fee,
            ),
            Self::InterestBearingConfig {
                rate_authority,
                rate,
            } => interest_bearing_mint::instruction::initialize(
                token_program_id,
                mint,
                rate_authority,
                rate,
            ),
            Self::NonTransferable => {
                instruction::initialize_non_transferable_mint(token_program_id, mint)
            }
            Self::PermanentDelegate { delegate } => {
                instruction::initialize_permanent_delegate(token_program_id, mint, &delegate)
            }
            Self::TransferHook {
                authority,
                program_id,
            } => transfer_hook::instruction::initialize(
                token_program_id,
                mint,
                authority,
                program_id,
            ),
            Self::MetadataPointer {
                authority,
                metadata_address,
            } => metadata_pointer::instruction::initialize(
                token_program_id,
                mint,
                authority,
                metadata_address,
            ),
            Self::ConfidentialTransferFeeConfig {
                authority,
                withdraw_withheld_authority_elgamal_pubkey,
            } => {
                confidential_transfer_fee::instruction::initialize_confidential_transfer_fee_config(
                    token_program_id,
                    mint,
                    authority,
                    withdraw_withheld_authority_elgamal_pubkey,
                )
            }
            Self::GroupPointer {
                authority,
                group_address,
            } => group_pointer::instruction::initialize(
                token_program_id,
                mint,
                authority,
                group_address,
            ),
            Self::GroupMemberPointer {
                authority,
                member_address,
            } => group_member_pointer::instruction::initialize(
                token_program_id,
                mint,
                authority,
                member_address,
            ),
        }
    }
}

pub type TokenResult<T> = Result<T, TokenError>;

#[derive(Debug)]
struct TokenMemo {
    text: String,
    signers: Vec<Pubkey>,
}
impl TokenMemo {
    pub fn to_instruction(&self) -> Instruction {
        spl_memo::build_memo(
            self.text.as_bytes(),
            &self.signers.iter().collect::<Vec<_>>(),
        )
    }
}

pub struct Token<T> {
    client: Arc<dyn ProgramClient<T>>,
    pubkey: Pubkey, /* token mint */
    decimals: Option<u8>,
    payer: Arc<dyn Signer>,
    program_id: Pubkey,
    nonce_account: Option<Pubkey>,
    nonce_authority: Option<Arc<dyn Signer>>,
    nonce_blockhash: Option<Hash>,
    memo: Arc<RwLock<Option<TokenMemo>>>,
    transfer_hook_accounts: Option<Vec<AccountMeta>>,
}

impl<T> fmt::Debug for Token<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Token")
            .field("pubkey", &self.pubkey)
            .field("decimals", &self.decimals)
            .field("payer", &self.payer.pubkey())
            .field("program_id", &self.program_id)
            .field("nonce_account", &self.nonce_account)
            .field(
                "nonce_authority",
                &self.nonce_authority.as_ref().map(|s| s.pubkey()),
            )
            .field("nonce_blockhash", &self.nonce_blockhash)
            .field("memo", &self.memo.read().unwrap())
            .field("transfer_hook_accounts", &self.transfer_hook_accounts)
            .finish()
    }
}

fn native_mint(program_id: &Pubkey) -> Pubkey {
    if program_id == &spl_token_2022::id() {
        spl_token_2022::native_mint::id()
    } else if program_id == &spl_token::id() {
        spl_token::native_mint::id()
    } else {
        panic!("Unrecognized token program id: {}", program_id);
    }
}

fn native_mint_decimals(program_id: &Pubkey) -> u8 {
    if program_id == &spl_token_2022::id() {
        spl_token_2022::native_mint::DECIMALS
    } else if program_id == &spl_token::id() {
        spl_token::native_mint::DECIMALS
    } else {
        panic!("Unrecognized token program id: {}", program_id);
    }
}

impl<T> Token<T>
where
    T: SendTransaction + SimulateTransaction,
{
    pub fn new(
        client: Arc<dyn ProgramClient<T>>,
        program_id: &Pubkey,
        address: &Pubkey,
        decimals: Option<u8>,
        payer: Arc<dyn Signer>,
    ) -> Self {
        Token {
            client,
            pubkey: *address,
            decimals,
            payer,
            program_id: *program_id,
            nonce_account: None,
            nonce_authority: None,
            nonce_blockhash: None,
            memo: Arc::new(RwLock::new(None)),
            transfer_hook_accounts: None,
        }
    }

    pub fn new_native(
        client: Arc<dyn ProgramClient<T>>,
        program_id: &Pubkey,
        payer: Arc<dyn Signer>,
    ) -> Self {
        Self::new(
            client,
            program_id,
            &native_mint(program_id),
            Some(native_mint_decimals(program_id)),
            payer,
        )
    }

    pub fn is_native(&self) -> bool {
        self.pubkey == native_mint(&self.program_id)
    }

    /// Get token address.
    pub fn get_address(&self) -> &Pubkey {
        &self.pubkey
    }

    pub fn with_payer(mut self, payer: Arc<dyn Signer>) -> Self {
        self.payer = payer;
        self
    }

    pub fn with_nonce(
        mut self,
        nonce_account: &Pubkey,
        nonce_authority: Arc<dyn Signer>,
        nonce_blockhash: &Hash,
    ) -> Self {
        self.nonce_account = Some(*nonce_account);
        self.nonce_authority = Some(nonce_authority);
        self.nonce_blockhash = Some(*nonce_blockhash);
        self.transfer_hook_accounts = Some(vec![]);
        self
    }

    pub fn with_transfer_hook_accounts(mut self, transfer_hook_accounts: Vec<AccountMeta>) -> Self {
        self.transfer_hook_accounts = Some(transfer_hook_accounts);
        self
    }

    pub fn with_memo<M: AsRef<str>>(&self, memo: M, signers: Vec<Pubkey>) -> &Self {
        let mut w_memo = self.memo.write().unwrap();
        *w_memo = Some(TokenMemo {
            text: memo.as_ref().to_string(),
            signers,
        });
        self
    }

    pub async fn get_new_latest_blockhash(&self) -> TokenResult<Hash> {
        let blockhash = self
            .client
            .get_latest_blockhash()
            .await
            .map_err(TokenError::Client)?;
        let start = Instant::now();
        let mut num_retries = 0;
        while start.elapsed().as_secs() < 5 {
            let new_blockhash = self
                .client
                .get_latest_blockhash()
                .await
                .map_err(TokenError::Client)?;
            if new_blockhash != blockhash {
                return Ok(new_blockhash);
            }

            time::sleep(Duration::from_millis(200)).await;
            num_retries += 1;
        }

        Err(TokenError::Client(Box::new(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "Unable to get new blockhash after {}ms (retried {} times), stuck at {}",
                start.elapsed().as_millis(),
                num_retries,
                blockhash
            ),
        ))))
    }

    fn get_multisig_signers<'a>(
        &self,
        authority: &Pubkey,
        signing_pubkeys: &'a [Pubkey],
    ) -> Vec<&'a Pubkey> {
        if signing_pubkeys == [*authority] {
            vec![]
        } else {
            signing_pubkeys.iter().collect::<Vec<_>>()
        }
    }

    async fn construct_tx<S: Signers>(
        &self,
        token_instructions: &[Instruction],
        additional_compute_budget: Option<u32>,
        signing_keypairs: &S,
    ) -> TokenResult<Transaction> {
        let mut instructions = vec![];
        let payer_key = self.payer.pubkey();
        let fee_payer = Some(&payer_key);

        {
            let mut w_memo = self.memo.write().unwrap();
            if let Some(memo) = w_memo.take() {
                let signing_pubkeys = signing_keypairs.pubkeys();
                if !memo
                    .signers
                    .iter()
                    .all(|signer| signing_pubkeys.contains(signer))
                {
                    return Err(TokenError::MissingMemoSigner);
                }

                instructions.push(memo.to_instruction());
            }
        }

        instructions.extend_from_slice(token_instructions);

        if let Some(additional_compute_budget) = additional_compute_budget {
            instructions.push(
                solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(
                    additional_compute_budget,
                ),
            );
        }

        let (message, blockhash) =
            if let (Some(nonce_account), Some(nonce_authority), Some(nonce_blockhash)) = (
                self.nonce_account,
                &self.nonce_authority,
                self.nonce_blockhash,
            ) {
                let mut message = Message::new_with_nonce(
                    token_instructions.to_vec(),
                    fee_payer,
                    &nonce_account,
                    &nonce_authority.pubkey(),
                );
                message.recent_blockhash = nonce_blockhash;
                (message, nonce_blockhash)
            } else {
                let latest_blockhash = self
                    .client
                    .get_latest_blockhash()
                    .await
                    .map_err(TokenError::Client)?;
                (
                    Message::new_with_blockhash(&instructions, fee_payer, &latest_blockhash),
                    latest_blockhash,
                )
            };

        let mut transaction = Transaction::new_unsigned(message);

        transaction
            .try_partial_sign(&vec![self.payer.clone()], blockhash)
            .map_err(|error| TokenError::Client(error.into()))?;
        if let Some(nonce_authority) = &self.nonce_authority {
            transaction
                .try_partial_sign(&vec![nonce_authority.clone()], blockhash)
                .map_err(|error| TokenError::Client(error.into()))?;
        }
        transaction
            .try_partial_sign(signing_keypairs, blockhash)
            .map_err(|error| TokenError::Client(error.into()))?;

        Ok(transaction)
    }

    pub async fn simulate_ixs<S: Signers>(
        &self,
        token_instructions: &[Instruction],
        signing_keypairs: &S,
    ) -> TokenResult<T::SimulationOutput> {
        let transaction = self
            .construct_tx(token_instructions, None, signing_keypairs)
            .await?;

        self.client
            .simulate_transaction(&transaction)
            .await
            .map_err(TokenError::Client)
    }

    pub async fn process_ixs<S: Signers>(
        &self,
        token_instructions: &[Instruction],
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let transaction = self
            .construct_tx(token_instructions, None, signing_keypairs)
            .await?;

        self.client
            .send_transaction(&transaction)
            .await
            .map_err(TokenError::Client)
    }

    pub async fn process_ixs_with_additional_compute_budget<S: Signers>(
        &self,
        token_instructions: &[Instruction],
        additional_compute_budget: u32,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let transaction = self
            .construct_tx(
                token_instructions,
                Some(additional_compute_budget),
                signing_keypairs,
            )
            .await?;

        self.client
            .send_transaction(&transaction)
            .await
            .map_err(TokenError::Client)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_mint<'a, S: Signers>(
        &self,
        mint_authority: &'a Pubkey,
        freeze_authority: Option<&'a Pubkey>,
        extension_initialization_params: Vec<ExtensionInitializationParams>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let decimals = self.decimals.ok_or(TokenError::MissingDecimals)?;

        let extension_types = extension_initialization_params
            .iter()
            .map(|e| e.extension())
            .collect::<Vec<_>>();
        let space = ExtensionType::try_calculate_account_len::<Mint>(&extension_types)?;

        let mut instructions = vec![system_instruction::create_account(
            &self.payer.pubkey(),
            &self.pubkey,
            self.client
                .get_minimum_balance_for_rent_exemption(space)
                .await
                .map_err(TokenError::Client)?,
            space as u64,
            &self.program_id,
        )];

        for params in extension_initialization_params {
            instructions.push(params.instruction(&self.program_id, &self.pubkey)?);
        }

        instructions.push(instruction::initialize_mint(
            &self.program_id,
            &self.pubkey,
            mint_authority,
            freeze_authority,
            decimals,
        )?);

        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Create native mint
    pub async fn create_native_mint(
        client: Arc<dyn ProgramClient<T>>,
        program_id: &Pubkey,
        payer: Arc<dyn Signer>,
    ) -> TokenResult<Self> {
        let token = Self::new_native(client, program_id, payer);
        token
            .process_ixs::<[&dyn Signer; 0]>(
                &[instruction::create_native_mint(
                    program_id,
                    &token.payer.pubkey(),
                )?],
                &[],
            )
            .await?;

        Ok(token)
    }

    /// Create multisig
    pub async fn create_multisig(
        &self,
        account: &dyn Signer,
        multisig_members: &[&Pubkey],
        minimum_signers: u8,
    ) -> TokenResult<T::Output> {
        let instructions = vec![
            system_instruction::create_account(
                &self.payer.pubkey(),
                &account.pubkey(),
                self.client
                    .get_minimum_balance_for_rent_exemption(Multisig::LEN)
                    .await
                    .map_err(TokenError::Client)?,
                Multisig::LEN as u64,
                &self.program_id,
            ),
            instruction::initialize_multisig(
                &self.program_id,
                &account.pubkey(),
                multisig_members,
                minimum_signers,
            )?,
        ];

        self.process_ixs(&instructions, &[account]).await
    }

    /// Get the address for the associated token account.
    pub fn get_associated_token_address(&self, owner: &Pubkey) -> Pubkey {
        get_associated_token_address_with_program_id(owner, &self.pubkey, &self.program_id)
    }

    /// Create and initialize the associated account.
    pub async fn create_associated_token_account(&self, owner: &Pubkey) -> TokenResult<T::Output> {
        self.process_ixs::<[&dyn Signer; 0]>(
            &[create_associated_token_account(
                &self.payer.pubkey(),
                owner,
                &self.pubkey,
                &self.program_id,
            )],
            &[],
        )
        .await
    }

    /// Create and initialize a new token account.
    pub async fn create_auxiliary_token_account(
        &self,
        account: &dyn Signer,
        owner: &Pubkey,
    ) -> TokenResult<T::Output> {
        self.create_auxiliary_token_account_with_extension_space(account, owner, vec![])
            .await
    }

    /// Create and initialize a new token account.
    pub async fn create_auxiliary_token_account_with_extension_space(
        &self,
        account: &dyn Signer,
        owner: &Pubkey,
        extensions: Vec<ExtensionType>,
    ) -> TokenResult<T::Output> {
        let state = self.get_mint_info().await?;
        let mint_extensions: Vec<ExtensionType> = state.get_extension_types()?;
        let mut required_extensions =
            ExtensionType::get_required_init_account_extensions(&mint_extensions);
        for extension_type in extensions.into_iter() {
            if !required_extensions.contains(&extension_type) {
                required_extensions.push(extension_type);
            }
        }
        let space = ExtensionType::try_calculate_account_len::<Account>(&required_extensions)?;
        let mut instructions = vec![system_instruction::create_account(
            &self.payer.pubkey(),
            &account.pubkey(),
            self.client
                .get_minimum_balance_for_rent_exemption(space)
                .await
                .map_err(TokenError::Client)?,
            space as u64,
            &self.program_id,
        )];

        if required_extensions.contains(&ExtensionType::ImmutableOwner) {
            instructions.push(instruction::initialize_immutable_owner(
                &self.program_id,
                &account.pubkey(),
            )?)
        }

        instructions.push(instruction::initialize_account(
            &self.program_id,
            &account.pubkey(),
            &self.pubkey,
            owner,
        )?);

        self.process_ixs(&instructions, &[account]).await
    }

    /// Retrieve a raw account
    pub async fn get_account(&self, account: Pubkey) -> TokenResult<BaseAccount> {
        self.client
            .get_account(account)
            .await
            .map_err(TokenError::Client)?
            .ok_or(TokenError::AccountNotFound)
    }

    fn unpack_mint_info(
        &self,
        account: BaseAccount,
    ) -> TokenResult<StateWithExtensionsOwned<Mint>> {
        if account.owner != self.program_id {
            return Err(TokenError::AccountInvalidOwner);
        }

        let mint_result =
            StateWithExtensionsOwned::<Mint>::unpack(account.data).map_err(Into::into);

        if let (Ok(mint), Some(decimals)) = (&mint_result, self.decimals) {
            if decimals != mint.base.decimals {
                return Err(TokenError::InvalidDecimals);
            }
        }

        mint_result
    }

    /// Retrive mint information.
    pub async fn get_mint_info(&self) -> TokenResult<StateWithExtensionsOwned<Mint>> {
        let account = self.get_account(self.pubkey).await?;
        self.unpack_mint_info(account)
    }

    /// Retrieve account information.
    pub async fn get_account_info(
        &self,
        account: &Pubkey,
    ) -> TokenResult<StateWithExtensionsOwned<Account>> {
        let account = self.get_account(*account).await?;
        if account.owner != self.program_id {
            return Err(TokenError::AccountInvalidOwner);
        }
        let account = StateWithExtensionsOwned::<Account>::unpack(account.data)?;
        if account.base.mint != *self.get_address() {
            return Err(TokenError::AccountInvalidMint);
        }

        Ok(account)
    }

    /// Retrieve the associated account or create one if not found.
    pub async fn get_or_create_associated_account_info(
        &self,
        owner: &Pubkey,
    ) -> TokenResult<StateWithExtensionsOwned<Account>> {
        let account = self.get_associated_token_address(owner);
        match self.get_account_info(&account).await {
            Ok(account) => Ok(account),
            // AccountInvalidOwner is possible if account already received some lamports.
            Err(TokenError::AccountNotFound) | Err(TokenError::AccountInvalidOwner) => {
                self.create_associated_token_account(owner).await?;
                self.get_account_info(&account).await
            }
            Err(error) => Err(error),
        }
    }

    /// Assign a new authority to the account.
    pub async fn set_authority<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        new_authority: Option<&Pubkey>,
        authority_type: instruction::AuthorityType,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[instruction::set_authority(
                &self.program_id,
                account,
                new_authority,
                authority_type,
                authority,
                &multisig_signers,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Mint new tokens
    pub async fn mint_to<S: Signers>(
        &self,
        destination: &Pubkey,
        authority: &Pubkey,
        amount: u64,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        let instructions = if let Some(decimals) = self.decimals {
            [instruction::mint_to_checked(
                &self.program_id,
                &self.pubkey,
                destination,
                authority,
                &multisig_signers,
                amount,
                decimals,
            )?]
        } else {
            [instruction::mint_to(
                &self.program_id,
                &self.pubkey,
                destination,
                authority,
                &multisig_signers,
                amount,
            )?]
        };

        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Transfer tokens to another account
    #[allow(clippy::too_many_arguments)]
    pub async fn transfer<S: Signers>(
        &self,
        source: &Pubkey,
        destination: &Pubkey,
        authority: &Pubkey,
        amount: u64,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        let fetch_account_data_fn = |address| {
            self.client
                .get_account(address)
                .map_ok(|opt| opt.map(|acc| acc.data))
        };

        let instruction = if let Some(decimals) = self.decimals {
            if let Some(transfer_hook_accounts) = &self.transfer_hook_accounts {
                let mut instruction = instruction::transfer_checked(
                    &self.program_id,
                    source,
                    self.get_address(),
                    destination,
                    authority,
                    &multisig_signers,
                    amount,
                    decimals,
                )?;
                instruction.accounts.extend(transfer_hook_accounts.clone());
                instruction
            } else {
                offchain::create_transfer_checked_instruction_with_extra_metas(
                    &self.program_id,
                    source,
                    self.get_address(),
                    destination,
                    authority,
                    &multisig_signers,
                    amount,
                    decimals,
                    fetch_account_data_fn,
                )
                .await
                .map_err(|_| TokenError::AccountNotFound)?
            }
        } else {
            #[allow(deprecated)]
            instruction::transfer(
                &self.program_id,
                source,
                destination,
                authority,
                &multisig_signers,
                amount,
            )?
        };

        self.process_ixs(&[instruction], signing_keypairs).await
    }

    /// Transfer tokens to an associated account, creating it if it does not
    /// exist
    #[allow(clippy::too_many_arguments)]
    pub async fn create_recipient_associated_account_and_transfer<S: Signers>(
        &self,
        source: &Pubkey,
        destination: &Pubkey,
        destination_owner: &Pubkey,
        authority: &Pubkey,
        amount: u64,
        fee: Option<u64>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        let fetch_account_data_fn = |address| {
            self.client
                .get_account(address)
                .map_ok(|opt| opt.map(|acc| acc.data))
        };

        if *destination != self.get_associated_token_address(destination_owner) {
            return Err(TokenError::AccountInvalidAssociatedAddress);
        }

        let mut instructions = vec![
            (create_associated_token_account_idempotent(
                &self.payer.pubkey(),
                destination_owner,
                &self.pubkey,
                &self.program_id,
            )),
        ];

        if let Some(fee) = fee {
            let decimals = self.decimals.ok_or(TokenError::MissingDecimals)?;
            instructions.push(transfer_fee::instruction::transfer_checked_with_fee(
                &self.program_id,
                source,
                &self.pubkey,
                destination,
                authority,
                &multisig_signers,
                amount,
                decimals,
                fee,
            )?);
        } else if let Some(decimals) = self.decimals {
            instructions.push(
                if let Some(transfer_hook_accounts) = &self.transfer_hook_accounts {
                    let mut instruction = instruction::transfer_checked(
                        &self.program_id,
                        source,
                        self.get_address(),
                        destination,
                        authority,
                        &multisig_signers,
                        amount,
                        decimals,
                    )?;
                    instruction.accounts.extend(transfer_hook_accounts.clone());
                    instruction
                } else {
                    offchain::create_transfer_checked_instruction_with_extra_metas(
                        &self.program_id,
                        source,
                        self.get_address(),
                        destination,
                        authority,
                        &multisig_signers,
                        amount,
                        decimals,
                        fetch_account_data_fn,
                    )
                    .await
                    .map_err(|_| TokenError::AccountNotFound)?
                },
            );
        } else {
            #[allow(deprecated)]
            instructions.push(instruction::transfer(
                &self.program_id,
                source,
                destination,
                authority,
                &multisig_signers,
                amount,
            )?);
        }

        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Transfer tokens to another account, given an expected fee
    #[allow(clippy::too_many_arguments)]
    pub async fn transfer_with_fee<S: Signers>(
        &self,
        source: &Pubkey,
        destination: &Pubkey,
        authority: &Pubkey,
        amount: u64,
        fee: u64,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);
        let decimals = self.decimals.ok_or(TokenError::MissingDecimals)?;

        self.process_ixs(
            &[transfer_fee::instruction::transfer_checked_with_fee(
                &self.program_id,
                source,
                &self.pubkey,
                destination,
                authority,
                &multisig_signers,
                amount,
                decimals,
                fee,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Burn tokens from account
    pub async fn burn<S: Signers>(
        &self,
        source: &Pubkey,
        authority: &Pubkey,
        amount: u64,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        let instructions = if let Some(decimals) = self.decimals {
            [instruction::burn_checked(
                &self.program_id,
                source,
                &self.pubkey,
                authority,
                &multisig_signers,
                amount,
                decimals,
            )?]
        } else {
            [instruction::burn(
                &self.program_id,
                source,
                &self.pubkey,
                authority,
                &multisig_signers,
                amount,
            )?]
        };

        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Approve a delegate to spend tokens
    pub async fn approve<S: Signers>(
        &self,
        source: &Pubkey,
        delegate: &Pubkey,
        authority: &Pubkey,
        amount: u64,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        let instructions = if let Some(decimals) = self.decimals {
            [instruction::approve_checked(
                &self.program_id,
                source,
                &self.pubkey,
                delegate,
                authority,
                &multisig_signers,
                amount,
                decimals,
            )?]
        } else {
            [instruction::approve(
                &self.program_id,
                source,
                delegate,
                authority,
                &multisig_signers,
                amount,
            )?]
        };

        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Revoke a delegate
    pub async fn revoke<S: Signers>(
        &self,
        source: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[instruction::revoke(
                &self.program_id,
                source,
                authority,
                &multisig_signers,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Close an empty account and reclaim its lamports
    pub async fn close_account<S: Signers>(
        &self,
        account: &Pubkey,
        lamports_destination: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        let mut instructions = vec![instruction::close_account(
            &self.program_id,
            account,
            lamports_destination,
            authority,
            &multisig_signers,
        )?];

        if let Ok(Some(destination_account)) = self.client.get_account(*lamports_destination).await
        {
            if let Ok(destination_obj) =
                StateWithExtensionsOwned::<Account>::unpack(destination_account.data)
            {
                if destination_obj.base.is_native() {
                    instructions.push(instruction::sync_native(
                        &self.program_id,
                        lamports_destination,
                    )?);
                }
            }
        }

        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Close an account, reclaiming its lamports and tokens
    pub async fn empty_and_close_account<S: Signers>(
        &self,
        account_to_close: &Pubkey,
        lamports_destination: &Pubkey,
        tokens_destination: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        // this implicitly validates that the mint on self is correct
        let account_state = self.get_account_info(account_to_close).await?;

        let mut instructions = vec![];

        if !self.is_native() && account_state.base.amount > 0 {
            // if a separate close authority is being used, it must be a delegate also
            if let Some(decimals) = self.decimals {
                instructions.push(instruction::transfer_checked(
                    &self.program_id,
                    account_to_close,
                    &self.pubkey,
                    tokens_destination,
                    authority,
                    &multisig_signers,
                    account_state.base.amount,
                    decimals,
                )?);
            } else {
                #[allow(deprecated)]
                instructions.push(instruction::transfer(
                    &self.program_id,
                    account_to_close,
                    tokens_destination,
                    authority,
                    &multisig_signers,
                    account_state.base.amount,
                )?);
            }
        }

        instructions.push(instruction::close_account(
            &self.program_id,
            account_to_close,
            lamports_destination,
            authority,
            &multisig_signers,
        )?);

        if let Ok(Some(destination_account)) = self.client.get_account(*lamports_destination).await
        {
            if let Ok(destination_obj) =
                StateWithExtensionsOwned::<Account>::unpack(destination_account.data)
            {
                if destination_obj.base.is_native() {
                    instructions.push(instruction::sync_native(
                        &self.program_id,
                        lamports_destination,
                    )?);
                }
            }
        }

        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Freeze a token account
    pub async fn freeze<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[instruction::freeze_account(
                &self.program_id,
                account,
                &self.pubkey,
                authority,
                &multisig_signers,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Thaw / unfreeze a token account
    pub async fn thaw<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[instruction::thaw_account(
                &self.program_id,
                account,
                &self.pubkey,
                authority,
                &multisig_signers,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Wrap lamports into native account
    pub async fn wrap<S: Signers>(
        &self,
        account: &Pubkey,
        owner: &Pubkey,
        lamports: u64,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        // mutable owner for Tokenkeg, immutable otherwise
        let immutable_owner = self.program_id != spl_token::id();
        let instructions = self.wrap_ixs(account, owner, lamports, immutable_owner)?;

        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Wrap lamports into a native account that can always have its ownership
    /// changed
    pub async fn wrap_with_mutable_ownership<S: Signers>(
        &self,
        account: &Pubkey,
        owner: &Pubkey,
        lamports: u64,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let instructions = self.wrap_ixs(account, owner, lamports, false)?;

        self.process_ixs(&instructions, signing_keypairs).await
    }

    fn wrap_ixs(
        &self,
        account: &Pubkey,
        owner: &Pubkey,
        lamports: u64,
        immutable_owner: bool,
    ) -> TokenResult<Vec<Instruction>> {
        if !self.is_native() {
            return Err(TokenError::AccountInvalidMint);
        }

        let mut instructions = vec![];
        if *account == self.get_associated_token_address(owner) {
            instructions.push(system_instruction::transfer(owner, account, lamports));
            instructions.push(create_associated_token_account(
                &self.payer.pubkey(),
                owner,
                &self.pubkey,
                &self.program_id,
            ));
        } else {
            let extensions = if immutable_owner {
                vec![ExtensionType::ImmutableOwner]
            } else {
                vec![]
            };
            let space = ExtensionType::try_calculate_account_len::<Account>(&extensions)?;

            instructions.push(system_instruction::create_account(
                &self.payer.pubkey(),
                account,
                lamports,
                space as u64,
                &self.program_id,
            ));

            if immutable_owner {
                instructions.push(instruction::initialize_immutable_owner(
                    &self.program_id,
                    account,
                )?)
            }

            instructions.push(instruction::initialize_account(
                &self.program_id,
                account,
                &self.pubkey,
                owner,
            )?);
        };

        Ok(instructions)
    }

    /// Sync native account lamports
    pub async fn sync_native(&self, account: &Pubkey) -> TokenResult<T::Output> {
        self.process_ixs::<[&dyn Signer; 0]>(
            &[instruction::sync_native(&self.program_id, account)?],
            &[],
        )
        .await
    }

    /// Set transfer fee
    pub async fn set_transfer_fee<S: Signers>(
        &self,
        authority: &Pubkey,
        transfer_fee_basis_points: u16,
        maximum_fee: u64,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[transfer_fee::instruction::set_transfer_fee(
                &self.program_id,
                &self.pubkey,
                authority,
                &multisig_signers,
                transfer_fee_basis_points,
                maximum_fee,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Set default account state on mint
    pub async fn set_default_account_state<S: Signers>(
        &self,
        authority: &Pubkey,
        state: &AccountState,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[
                default_account_state::instruction::update_default_account_state(
                    &self.program_id,
                    &self.pubkey,
                    authority,
                    &multisig_signers,
                    state,
                )?,
            ],
            signing_keypairs,
        )
        .await
    }

    /// Harvest withheld tokens to mint
    pub async fn harvest_withheld_tokens_to_mint(
        &self,
        sources: &[&Pubkey],
    ) -> TokenResult<T::Output> {
        self.process_ixs::<[&dyn Signer; 0]>(
            &[transfer_fee::instruction::harvest_withheld_tokens_to_mint(
                &self.program_id,
                &self.pubkey,
                sources,
            )?],
            &[],
        )
        .await
    }

    /// Withdraw withheld tokens from mint
    pub async fn withdraw_withheld_tokens_from_mint<S: Signers>(
        &self,
        destination: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[
                transfer_fee::instruction::withdraw_withheld_tokens_from_mint(
                    &self.program_id,
                    &self.pubkey,
                    destination,
                    authority,
                    &multisig_signers,
                )?,
            ],
            signing_keypairs,
        )
        .await
    }

    /// Withdraw withheld tokens from accounts
    pub async fn withdraw_withheld_tokens_from_accounts<S: Signers>(
        &self,
        destination: &Pubkey,
        authority: &Pubkey,
        sources: &[&Pubkey],
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[
                transfer_fee::instruction::withdraw_withheld_tokens_from_accounts(
                    &self.program_id,
                    &self.pubkey,
                    destination,
                    authority,
                    &multisig_signers,
                    sources,
                )?,
            ],
            signing_keypairs,
        )
        .await
    }

    /// Reallocate a token account to be large enough for a set of
    /// ExtensionTypes
    pub async fn reallocate<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        extension_types: &[ExtensionType],
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[instruction::reallocate(
                &self.program_id,
                account,
                &self.payer.pubkey(),
                authority,
                &multisig_signers,
                extension_types,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Require memos on transfers into this account
    pub async fn enable_required_transfer_memos<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[memo_transfer::instruction::enable_required_transfer_memos(
                &self.program_id,
                account,
                authority,
                &multisig_signers,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Stop requiring memos on transfers into this account
    pub async fn disable_required_transfer_memos<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[memo_transfer::instruction::disable_required_transfer_memos(
                &self.program_id,
                account,
                authority,
                &multisig_signers,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Prevent unsafe usage of token account through CPI
    pub async fn enable_cpi_guard<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[cpi_guard::instruction::enable_cpi_guard(
                &self.program_id,
                account,
                authority,
                &multisig_signers,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Stop preventing unsafe usage of token account through CPI
    pub async fn disable_cpi_guard<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[cpi_guard::instruction::disable_cpi_guard(
                &self.program_id,
                account,
                authority,
                &multisig_signers,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Update interest rate
    pub async fn update_interest_rate<S: Signers>(
        &self,
        authority: &Pubkey,
        new_rate: i16,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[interest_bearing_mint::instruction::update_rate(
                &self.program_id,
                self.get_address(),
                authority,
                &multisig_signers,
                new_rate,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Update transfer hook program id
    pub async fn update_transfer_hook_program_id<S: Signers>(
        &self,
        authority: &Pubkey,
        new_program_id: Option<Pubkey>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[transfer_hook::instruction::update(
                &self.program_id,
                self.get_address(),
                authority,
                &multisig_signers,
                new_program_id,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Update metadata pointer address
    pub async fn update_metadata_address<S: Signers>(
        &self,
        authority: &Pubkey,
        new_metadata_address: Option<Pubkey>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[metadata_pointer::instruction::update(
                &self.program_id,
                self.get_address(),
                authority,
                &multisig_signers,
                new_metadata_address,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Update group pointer address
    pub async fn update_group_address<S: Signers>(
        &self,
        authority: &Pubkey,
        new_group_address: Option<Pubkey>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[group_pointer::instruction::update(
                &self.program_id,
                self.get_address(),
                authority,
                &multisig_signers,
                new_group_address,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Update group member pointer address
    pub async fn update_group_member_address<S: Signers>(
        &self,
        authority: &Pubkey,
        new_member_address: Option<Pubkey>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[group_member_pointer::instruction::update(
                &self.program_id,
                self.get_address(),
                authority,
                &multisig_signers,
                new_member_address,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Update confidential transfer mint
    pub async fn confidential_transfer_update_mint<S: Signers>(
        &self,
        authority: &Pubkey,
        auto_approve_new_account: bool,
        auditor_elgamal_pubkey: Option<PodElGamalPubkey>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[confidential_transfer::instruction::update_mint(
                &self.program_id,
                &self.pubkey,
                authority,
                &multisig_signers,
                auto_approve_new_account,
                auditor_elgamal_pubkey,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Configures confidential transfers for a token account. If the maximum
    /// pending balance credit counter for the extension is not provided,
    /// then it is set to be a default value of `2^16`.
    #[allow(clippy::too_many_arguments)]
    pub async fn confidential_transfer_configure_token_account<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        context_state_account: Option<&Pubkey>,
        maximum_pending_balance_credit_counter: Option<u64>,
        elgamal_keypair: &ElGamalKeypair,
        aes_key: &AeKey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        const DEFAULT_MAXIMUM_PENDING_BALANCE_CREDIT_COUNTER: u64 = 65536;

        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        let maximum_pending_balance_credit_counter = maximum_pending_balance_credit_counter
            .unwrap_or(DEFAULT_MAXIMUM_PENDING_BALANCE_CREDIT_COUNTER);

        let proof_data = if context_state_account.is_some() {
            None
        } else {
            Some(
                confidential_transfer::instruction::PubkeyValidityData::new(elgamal_keypair)
                    .map_err(|_| TokenError::ProofGeneration)?,
            )
        };

        let proof_location = if let Some(proof_data_temp) = proof_data.as_ref() {
            ProofLocation::InstructionOffset(1.try_into().unwrap(), proof_data_temp)
        } else {
            let context_state_account = context_state_account.unwrap();
            ProofLocation::ContextStateAccount(context_state_account)
        };

        let decryptable_balance = aes_key.encrypt(0);

        self.process_ixs(
            &confidential_transfer::instruction::configure_account(
                &self.program_id,
                account,
                &self.pubkey,
                decryptable_balance,
                maximum_pending_balance_credit_counter,
                authority,
                &multisig_signers,
                proof_location,
            )?,
            signing_keypairs,
        )
        .await
    }

    /// Approves a token account for confidential transfers
    pub async fn confidential_transfer_approve_account<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[confidential_transfer::instruction::approve_account(
                &self.program_id,
                account,
                &self.pubkey,
                authority,
                &multisig_signers,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Prepare a token account with the confidential transfer extension for
    /// closing
    pub async fn confidential_transfer_empty_account<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        context_state_account: Option<&Pubkey>,
        account_info: Option<EmptyAccountAccountInfo>,
        elgamal_keypair: &ElGamalKeypair,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        let account_info = if let Some(account_info) = account_info {
            account_info
        } else {
            let account = self.get_account_info(account).await?;
            let confidential_transfer_account =
                account.get_extension::<ConfidentialTransferAccount>()?;
            EmptyAccountAccountInfo::new(confidential_transfer_account)
        };

        let proof_data = if context_state_account.is_some() {
            None
        } else {
            Some(
                account_info
                    .generate_proof_data(elgamal_keypair)
                    .map_err(|_| TokenError::ProofGeneration)?,
            )
        };

        let proof_location = if let Some(proof_data_temp) = proof_data.as_ref() {
            ProofLocation::InstructionOffset(1.try_into().unwrap(), proof_data_temp)
        } else {
            let context_state_account = context_state_account.unwrap();
            ProofLocation::ContextStateAccount(context_state_account)
        };

        self.process_ixs(
            &confidential_transfer::instruction::empty_account(
                &self.program_id,
                account,
                authority,
                &multisig_signers,
                proof_location,
            )?,
            signing_keypairs,
        )
        .await
    }

    /// Deposit SPL Tokens into the pending balance of a confidential token
    /// account
    pub async fn confidential_transfer_deposit<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        amount: u64,
        decimals: u8,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[confidential_transfer::instruction::deposit(
                &self.program_id,
                account,
                &self.pubkey,
                amount,
                decimals,
                authority,
                &multisig_signers,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Withdraw SPL Tokens from the available balance of a confidential token
    /// account
    #[allow(clippy::too_many_arguments)]
    pub async fn confidential_transfer_withdraw<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        context_state_account: Option<&Pubkey>,
        withdraw_amount: u64,
        decimals: u8,
        account_info: Option<WithdrawAccountInfo>,
        elgamal_keypair: &ElGamalKeypair,
        aes_key: &AeKey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        let account_info = if let Some(account_info) = account_info {
            account_info
        } else {
            let account = self.get_account_info(account).await?;
            let confidential_transfer_account =
                account.get_extension::<ConfidentialTransferAccount>()?;
            WithdrawAccountInfo::new(confidential_transfer_account)
        };

        let proof_data = if context_state_account.is_some() {
            None
        } else {
            Some(
                account_info
                    .generate_proof_data(withdraw_amount, elgamal_keypair, aes_key)
                    .map_err(|_| TokenError::ProofGeneration)?,
            )
        };

        let proof_location = if let Some(proof_data_temp) = proof_data.as_ref() {
            ProofLocation::InstructionOffset(1.try_into().unwrap(), proof_data_temp)
        } else {
            let context_state_account = context_state_account.unwrap();
            ProofLocation::ContextStateAccount(context_state_account)
        };

        let new_decryptable_available_balance = account_info
            .new_decryptable_available_balance(withdraw_amount, aes_key)
            .map_err(|_| TokenError::AccountDecryption)?;

        self.process_ixs(
            &confidential_transfer::instruction::withdraw(
                &self.program_id,
                account,
                &self.pubkey,
                withdraw_amount,
                decimals,
                new_decryptable_available_balance,
                authority,
                &multisig_signers,
                proof_location,
            )?,
            signing_keypairs,
        )
        .await
    }

    /// Create withdraw proof context state account for a confidential transfer
    /// withdraw instruction.
    pub async fn create_withdraw_proof_context_state<S: Signer>(
        &self,
        context_state_account: &Pubkey,
        context_state_authority: &Pubkey,
        withdraw_proof_data: &WithdrawData,
        withdraw_proof_signer: &S,
    ) -> TokenResult<T::Output> {
        // create withdraw proof context state
        let instruction_type = ProofInstruction::VerifyWithdraw;
        let space = size_of::<ProofContextState<WithdrawProofContext>>();
        let rent = self
            .client
            .get_minimum_balance_for_rent_exemption(space)
            .await
            .map_err(TokenError::Client)?;

        let withdraw_proof_context_state_info = ContextStateInfo {
            context_state_account,
            context_state_authority,
        };

        self.process_ixs(
            &[system_instruction::create_account(
                &self.payer.pubkey(),
                context_state_account,
                rent,
                space as u64,
                &zk_token_proof_program::id(),
            )],
            &[withdraw_proof_signer],
        )
        .await?;

        self.process_ixs(
            &[instruction_type
                .encode_verify_proof(Some(withdraw_proof_context_state_info), withdraw_proof_data)],
            &[] as &[&dyn Signer; 0],
        )
        .await
    }

    /// Transfer tokens confidentially
    #[allow(clippy::too_many_arguments)]
    pub async fn confidential_transfer_transfer<S: Signers>(
        &self,
        source_account: &Pubkey,
        destination_account: &Pubkey,
        source_authority: &Pubkey,
        context_state_account: Option<&Pubkey>,
        transfer_amount: u64,
        account_info: Option<TransferAccountInfo>,
        source_elgamal_keypair: &ElGamalKeypair,
        source_aes_key: &AeKey,
        destination_elgamal_pubkey: &ElGamalPubkey,
        auditor_elgamal_pubkey: Option<&ElGamalPubkey>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(source_authority, &signing_pubkeys);

        let account_info = if let Some(account_info) = account_info {
            account_info
        } else {
            let account = self.get_account_info(source_account).await?;
            let confidential_transfer_account =
                account.get_extension::<ConfidentialTransferAccount>()?;
            TransferAccountInfo::new(confidential_transfer_account)
        };

        let proof_data = if context_state_account.is_some() {
            None
        } else {
            Some(
                account_info
                    .generate_transfer_proof_data(
                        transfer_amount,
                        source_elgamal_keypair,
                        source_aes_key,
                        destination_elgamal_pubkey,
                        auditor_elgamal_pubkey,
                    )
                    .map_err(|_| TokenError::ProofGeneration)?,
            )
        };

        let proof_location = if let Some(proof_data_temp) = proof_data.as_ref() {
            ProofLocation::InstructionOffset(1.try_into().unwrap(), proof_data_temp)
        } else {
            let context_state_account = context_state_account.unwrap();
            ProofLocation::ContextStateAccount(context_state_account)
        };

        let new_decryptable_available_balance = account_info
            .new_decryptable_available_balance(transfer_amount, source_aes_key)
            .map_err(|_| TokenError::AccountDecryption)?;

        let mut instructions = confidential_transfer::instruction::transfer(
            &self.program_id,
            source_account,
            self.get_address(),
            destination_account,
            new_decryptable_available_balance,
            source_authority,
            &multisig_signers,
            proof_location,
        )?;
        offchain::add_extra_account_metas(
            &mut instructions[0],
            source_account,
            self.get_address(),
            destination_account,
            source_authority,
            u64::MAX,
            |address| {
                self.client
                    .get_account(address)
                    .map_ok(|opt| opt.map(|acc| acc.data))
            },
        )
        .await
        .map_err(|_| TokenError::AccountNotFound)?;
        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Transfer tokens confidentially using split proofs.
    ///
    /// This function assumes that proof context states have already been
    /// created.
    #[allow(clippy::too_many_arguments)]
    pub async fn confidential_transfer_transfer_with_split_proofs<S: Signers>(
        &self,
        source_account: &Pubkey,
        destination_account: &Pubkey,
        source_authority: &Pubkey,
        context_state_accounts: TransferSplitContextStateAccounts<'_>,
        transfer_amount: u64,
        account_info: Option<TransferAccountInfo>,
        source_aes_key: &AeKey,
        source_decrypt_handles: &SourceDecryptHandles,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let account_info = if let Some(account_info) = account_info {
            account_info
        } else {
            let account = self.get_account_info(source_account).await?;
            let confidential_transfer_account =
                account.get_extension::<ConfidentialTransferAccount>()?;
            TransferAccountInfo::new(confidential_transfer_account)
        };

        let new_decryptable_available_balance = account_info
            .new_decryptable_available_balance(transfer_amount, source_aes_key)
            .map_err(|_| TokenError::AccountDecryption)?;

        let mut instruction = confidential_transfer::instruction::transfer_with_split_proofs(
            &self.program_id,
            source_account,
            self.get_address(),
            destination_account,
            new_decryptable_available_balance.into(),
            source_authority,
            context_state_accounts,
            source_decrypt_handles,
        )?;
        offchain::add_extra_account_metas(
            &mut instruction,
            source_account,
            self.get_address(),
            destination_account,
            source_authority,
            u64::MAX,
            |address| {
                self.client
                    .get_account(address)
                    .map_ok(|opt| opt.map(|acc| acc.data))
            },
        )
        .await
        .map_err(|_| TokenError::AccountNotFound)?;
        self.process_ixs(&[instruction], signing_keypairs).await
    }

    /// Transfer tokens confidentially using split proofs in parallel
    ///
    /// This function internally generates the ZK Token proof instructions to
    /// create the necessary proof context states.
    #[allow(clippy::too_many_arguments)]
    pub async fn confidential_transfer_transfer_with_split_proofs_in_parallel<S: Signers>(
        &self,
        source_account: &Pubkey,
        destination_account: &Pubkey,
        source_authority: &Pubkey,
        context_state_accounts: TransferSplitContextStateAccounts<'_>,
        transfer_amount: u64,
        account_info: Option<TransferAccountInfo>,
        source_elgamal_keypair: &ElGamalKeypair,
        source_aes_key: &AeKey,
        destination_elgamal_pubkey: &ElGamalPubkey,
        auditor_elgamal_pubkey: Option<&ElGamalPubkey>,
        equality_and_ciphertext_validity_proof_signers: &S,
        range_proof_signers: &S,
    ) -> TokenResult<(T::Output, T::Output)> {
        let account_info = if let Some(account_info) = account_info {
            account_info
        } else {
            let account = self.get_account_info(source_account).await?;
            let confidential_transfer_account =
                account.get_extension::<ConfidentialTransferAccount>()?;
            TransferAccountInfo::new(confidential_transfer_account)
        };

        let (
            equality_proof_data,
            ciphertext_validity_proof_data,
            range_proof_data,
            source_decrypt_handles,
        ) = account_info
            .generate_split_transfer_proof_data(
                transfer_amount,
                source_elgamal_keypair,
                source_aes_key,
                destination_elgamal_pubkey,
                auditor_elgamal_pubkey,
            )
            .map_err(|_| TokenError::ProofGeneration)?;

        let new_decryptable_available_balance = account_info
            .new_decryptable_available_balance(transfer_amount, source_aes_key)
            .map_err(|_| TokenError::AccountDecryption)?;

        let mut transfer_instruction =
            confidential_transfer::instruction::transfer_with_split_proofs(
                &self.program_id,
                source_account,
                self.get_address(),
                destination_account,
                new_decryptable_available_balance.into(),
                source_authority,
                context_state_accounts,
                &source_decrypt_handles,
            )?;
        offchain::add_extra_account_metas(
            &mut transfer_instruction,
            source_account,
            self.get_address(),
            destination_account,
            source_authority,
            u64::MAX,
            |address| {
                self.client
                    .get_account(address)
                    .map_ok(|opt| opt.map(|acc| acc.data))
            },
        )
        .await
        .map_err(|_| TokenError::AccountNotFound)?;

        let transfer_with_equality_and_ciphertext_validity = self
            .create_equality_and_ciphertext_validity_proof_context_states_for_transfer_parallel(
                context_state_accounts,
                &equality_proof_data,
                &ciphertext_validity_proof_data,
                &transfer_instruction,
                equality_and_ciphertext_validity_proof_signers,
            );

        let transfer_with_range_proof = self
            .create_range_proof_context_state_for_transfer_parallel(
                context_state_accounts,
                &range_proof_data,
                &transfer_instruction,
                range_proof_signers,
            );

        try_join!(
            transfer_with_equality_and_ciphertext_validity,
            transfer_with_range_proof
        )
    }

    /// Create equality proof context state account for a confidential transfer.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_equality_proof_context_state_for_transfer<S: Signer>(
        &self,
        context_state_accounts: TransferSplitContextStateAccounts<'_>,
        equality_proof_data: &CiphertextCommitmentEqualityProofData,
        equality_proof_signer: &S,
    ) -> TokenResult<T::Output> {
        // create equality proof context state
        let instruction_type = ProofInstruction::VerifyCiphertextCommitmentEquality;
        let space = size_of::<ProofContextState<CiphertextCommitmentEqualityProofContext>>();
        let rent = self
            .client
            .get_minimum_balance_for_rent_exemption(space)
            .await
            .map_err(TokenError::Client)?;

        let equality_proof_context_state_info = ContextStateInfo {
            context_state_account: context_state_accounts.equality_proof,
            context_state_authority: context_state_accounts.authority,
        };

        self.process_ixs(
            &[
                system_instruction::create_account(
                    &self.payer.pubkey(),
                    context_state_accounts.equality_proof,
                    rent,
                    space as u64,
                    &zk_token_proof_program::id(),
                ),
                instruction_type.encode_verify_proof(
                    Some(equality_proof_context_state_info),
                    equality_proof_data,
                ),
            ],
            &[equality_proof_signer],
        )
        .await
    }

    /// Create ciphertext validity proof context state account for a
    /// confidential transfer.
    pub async fn create_ciphertext_validity_proof_context_state_for_transfer<S: Signer>(
        &self,
        context_state_accounts: TransferSplitContextStateAccounts<'_>,
        ciphertext_validity_proof_data: &BatchedGroupedCiphertext2HandlesValidityProofData,
        ciphertext_validity_proof_signer: &S,
    ) -> TokenResult<T::Output> {
        // create ciphertext validity proof context state
        let instruction_type = ProofInstruction::VerifyBatchedGroupedCiphertext2HandlesValidity;
        let space =
            size_of::<ProofContextState<BatchedGroupedCiphertext2HandlesValidityProofContext>>();
        let rent = self
            .client
            .get_minimum_balance_for_rent_exemption(space)
            .await
            .map_err(TokenError::Client)?;

        let ciphertext_validity_proof_context_state_info = ContextStateInfo {
            context_state_account: context_state_accounts.ciphertext_validity_proof,
            context_state_authority: context_state_accounts.authority,
        };

        self.process_ixs(
            &[
                system_instruction::create_account(
                    &self.payer.pubkey(),
                    context_state_accounts.ciphertext_validity_proof,
                    rent,
                    space as u64,
                    &zk_token_proof_program::id(),
                ),
                instruction_type.encode_verify_proof(
                    Some(ciphertext_validity_proof_context_state_info),
                    ciphertext_validity_proof_data,
                ),
            ],
            &[ciphertext_validity_proof_signer],
        )
        .await
    }

    /// Create equality and ciphertext validity proof context state accounts for
    /// a confidential transfer.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_equality_and_ciphertext_validity_proof_context_states_for_transfer<
        S: Signers,
    >(
        &self,
        context_state_accounts: TransferSplitContextStateAccounts<'_>,
        equality_proof_data: &CiphertextCommitmentEqualityProofData,
        ciphertext_validity_proof_data: &BatchedGroupedCiphertext2HandlesValidityProofData,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.create_equality_and_ciphertext_validity_proof_context_state_with_optional_transfer(
            context_state_accounts,
            equality_proof_data,
            ciphertext_validity_proof_data,
            None,
            signing_keypairs,
        )
        .await
    }

    /// Create equality and ciphertext validity proof context state accounts
    /// with a confidential transfer instruction.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_equality_and_ciphertext_validity_proof_context_states_for_transfer_parallel<
        S: Signers,
    >(
        &self,
        context_state_accounts: TransferSplitContextStateAccounts<'_>,
        equality_proof_data: &CiphertextCommitmentEqualityProofData,
        ciphertext_validity_proof_data: &BatchedGroupedCiphertext2HandlesValidityProofData,
        transfer_instruction: &Instruction,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.create_equality_and_ciphertext_validity_proof_context_state_with_optional_transfer(
            context_state_accounts,
            equality_proof_data,
            ciphertext_validity_proof_data,
            Some(transfer_instruction),
            signing_keypairs,
        )
        .await
    }

    /// Create equality and ciphertext validity proof context states for a
    /// confidential transfer.
    ///
    /// If an optional transfer instruction is provided, then the transfer
    /// instruction is attached to the same transaction.
    #[allow(clippy::too_many_arguments)]
    async fn create_equality_and_ciphertext_validity_proof_context_state_with_optional_transfer<
        S: Signers,
    >(
        &self,
        context_state_accounts: TransferSplitContextStateAccounts<'_>,
        equality_proof_data: &CiphertextCommitmentEqualityProofData,
        ciphertext_validity_proof_data: &BatchedGroupedCiphertext2HandlesValidityProofData,
        transfer_instruction: Option<&Instruction>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let mut instructions = vec![];

        // create equality proof context state
        let instruction_type = ProofInstruction::VerifyCiphertextCommitmentEquality;
        let space = size_of::<ProofContextState<CiphertextCommitmentEqualityProofContext>>();
        let rent = self
            .client
            .get_minimum_balance_for_rent_exemption(space)
            .await
            .map_err(TokenError::Client)?;
        instructions.push(system_instruction::create_account(
            &self.payer.pubkey(),
            context_state_accounts.equality_proof,
            rent,
            space as u64,
            &zk_token_proof_program::id(),
        ));

        let equality_proof_context_state_info = ContextStateInfo {
            context_state_account: context_state_accounts.equality_proof,
            context_state_authority: context_state_accounts.authority,
        };
        instructions.push(
            instruction_type
                .encode_verify_proof(Some(equality_proof_context_state_info), equality_proof_data),
        );

        // create ciphertext validity proof context state
        let instruction_type = ProofInstruction::VerifyBatchedGroupedCiphertext2HandlesValidity;
        let space =
            size_of::<ProofContextState<BatchedGroupedCiphertext2HandlesValidityProofContext>>();
        let rent = self
            .client
            .get_minimum_balance_for_rent_exemption(space)
            .await
            .map_err(TokenError::Client)?;
        instructions.push(system_instruction::create_account(
            &self.payer.pubkey(),
            context_state_accounts.ciphertext_validity_proof,
            rent,
            space as u64,
            &zk_token_proof_program::id(),
        ));

        let ciphertext_validity_proof_context_state_info = ContextStateInfo {
            context_state_account: context_state_accounts.ciphertext_validity_proof,
            context_state_authority: context_state_accounts.authority,
        };
        instructions.push(instruction_type.encode_verify_proof(
            Some(ciphertext_validity_proof_context_state_info),
            ciphertext_validity_proof_data,
        ));

        // add transfer instruction
        if let Some(transfer_instruction) = transfer_instruction {
            instructions.push(transfer_instruction.clone());
        }

        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Create a range proof context state account for a confidential transfer.
    pub async fn create_range_proof_context_state_for_transfer<S: Signer>(
        &self,
        context_state_accounts: TransferSplitContextStateAccounts<'_>,
        range_proof_data: &BatchedRangeProofU128Data,
        range_proof_keypair: &S,
    ) -> TokenResult<T::Output> {
        let instruction_type = ProofInstruction::VerifyBatchedRangeProofU128;
        let space = size_of::<ProofContextState<BatchedRangeProofContext>>();
        let rent = self
            .client
            .get_minimum_balance_for_rent_exemption(space)
            .await
            .map_err(TokenError::Client)?;
        let range_proof_context_state_info = ContextStateInfo {
            context_state_account: context_state_accounts.range_proof,
            context_state_authority: context_state_accounts.authority,
        };
        self.process_ixs(
            &[system_instruction::create_account(
                &self.payer.pubkey(),
                context_state_accounts.range_proof,
                rent,
                space as u64,
                &zk_token_proof_program::id(),
            )],
            &[range_proof_keypair],
        )
        .await?;

        // This instruction is right at the transaction size limit, but in the
        // future it might be able to support the transfer too
        self.process_ixs(
            &[instruction_type
                .encode_verify_proof(Some(range_proof_context_state_info), range_proof_data)],
            &[] as &[&dyn Signer; 0],
        )
        .await
    }

    /// Create a range proof context state account with a confidential transfer
    /// instruction.
    pub async fn create_range_proof_context_state_for_transfer_parallel<S: Signers>(
        &self,
        context_state_accounts: TransferSplitContextStateAccounts<'_>,
        range_proof_data: &BatchedRangeProofU128Data,
        transfer_instruction: &Instruction,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.create_range_proof_context_state_with_optional_transfer(
            context_state_accounts,
            range_proof_data,
            Some(transfer_instruction),
            signing_keypairs,
        )
        .await
    }

    /// Create a range proof context state account and an optional confidential
    /// transfer instruction.
    async fn create_range_proof_context_state_with_optional_transfer<S: Signers>(
        &self,
        context_state_accounts: TransferSplitContextStateAccounts<'_>,
        range_proof_data: &BatchedRangeProofU128Data,
        transfer_instruction: Option<&Instruction>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let instruction_type = ProofInstruction::VerifyBatchedRangeProofU128;
        let space = size_of::<ProofContextState<BatchedRangeProofContext>>();
        let rent = self
            .client
            .get_minimum_balance_for_rent_exemption(space)
            .await
            .map_err(TokenError::Client)?;
        let range_proof_context_state_info = ContextStateInfo {
            context_state_account: context_state_accounts.range_proof,
            context_state_authority: context_state_accounts.authority,
        };

        let mut instructions = vec![
            system_instruction::create_account(
                &self.payer.pubkey(),
                context_state_accounts.range_proof,
                rent,
                space as u64,
                &zk_token_proof_program::id(),
            ),
            instruction_type
                .encode_verify_proof(Some(range_proof_context_state_info), range_proof_data),
        ];

        if let Some(transfer_instruction) = transfer_instruction {
            instructions.push(transfer_instruction.clone());
        }

        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Close a ZK Token proof program context state
    pub async fn confidential_transfer_close_context_state<S: Signers>(
        &self,
        context_state_account: &Pubkey,
        lamport_destination_account: &Pubkey,
        context_state_authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let context_state_info = ContextStateInfo {
            context_state_account,
            context_state_authority,
        };

        self.process_ixs(
            &[zk_token_proof_instruction::close_context_state(
                context_state_info,
                lamport_destination_account,
            )],
            signing_keypairs,
        )
        .await
    }

    /// Transfer tokens confidentially with fee
    #[allow(clippy::too_many_arguments)]
    pub async fn confidential_transfer_transfer_with_fee<S: Signers>(
        &self,
        source_account: &Pubkey,
        destination_account: &Pubkey,
        source_authority: &Pubkey,
        context_state_account: Option<&Pubkey>,
        transfer_amount: u64,
        account_info: Option<TransferAccountInfo>,
        source_elgamal_keypair: &ElGamalKeypair,
        source_aes_key: &AeKey,
        destination_elgamal_pubkey: &ElGamalPubkey,
        auditor_elgamal_pubkey: Option<&ElGamalPubkey>,
        withdraw_withheld_authority_elgamal_pubkey: &ElGamalPubkey,
        fee_rate_basis_points: u16,
        maximum_fee: u64,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(source_authority, &signing_pubkeys);

        let account_info = if let Some(account_info) = account_info {
            account_info
        } else {
            let account = self.get_account_info(source_account).await?;
            let confidential_transfer_account =
                account.get_extension::<ConfidentialTransferAccount>()?;
            TransferAccountInfo::new(confidential_transfer_account)
        };

        let proof_data = if context_state_account.is_some() {
            None
        } else {
            Some(
                account_info
                    .generate_transfer_with_fee_proof_data(
                        transfer_amount,
                        source_elgamal_keypair,
                        source_aes_key,
                        destination_elgamal_pubkey,
                        auditor_elgamal_pubkey,
                        withdraw_withheld_authority_elgamal_pubkey,
                        fee_rate_basis_points,
                        maximum_fee,
                    )
                    .map_err(|_| TokenError::ProofGeneration)?,
            )
        };

        let proof_location = if let Some(proof_data_temp) = proof_data.as_ref() {
            ProofLocation::InstructionOffset(1.try_into().unwrap(), proof_data_temp)
        } else {
            let context_state_account = context_state_account.unwrap();
            ProofLocation::ContextStateAccount(context_state_account)
        };

        let new_decryptable_available_balance = account_info
            .new_decryptable_available_balance(transfer_amount, source_aes_key)
            .map_err(|_| TokenError::AccountDecryption)?;

        // additional compute budget required for `VerifyTransferWithFee`
        const TRANSFER_WITH_FEE_COMPUTE_BUDGET: u32 = 500_000;

        let mut instructions = confidential_transfer::instruction::transfer_with_fee(
            &self.program_id,
            source_account,
            destination_account,
            self.get_address(),
            new_decryptable_available_balance,
            source_authority,
            &multisig_signers,
            proof_location,
        )?;
        offchain::add_extra_account_metas(
            &mut instructions[0],
            source_account,
            self.get_address(),
            destination_account,
            source_authority,
            u64::MAX,
            |address| {
                self.client
                    .get_account(address)
                    .map_ok(|opt| opt.map(|acc| acc.data))
            },
        )
        .await
        .map_err(|_| TokenError::AccountNotFound)?;
        self.process_ixs_with_additional_compute_budget(
            &instructions,
            TRANSFER_WITH_FEE_COMPUTE_BUDGET,
            signing_keypairs,
        )
        .await
    }

    /// Transfer tokens confidentially with fee using split proofs.
    ///
    /// This function assumes that proof context states have already been
    /// created.
    #[allow(clippy::too_many_arguments)]
    pub async fn confidential_transfer_transfer_with_fee_and_split_proofs<S: Signers>(
        &self,
        source_account: &Pubkey,
        destination_account: &Pubkey,
        source_authority: &Pubkey,
        context_state_accounts: TransferWithFeeSplitContextStateAccounts<'_>,
        transfer_amount: u64,
        account_info: Option<TransferAccountInfo>,
        source_aes_key: &AeKey,
        source_decrypt_handles: &SourceDecryptHandles,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let account_info = if let Some(account_info) = account_info {
            account_info
        } else {
            let account = self.get_account_info(source_account).await?;
            let confidential_transfer_account =
                account.get_extension::<ConfidentialTransferAccount>()?;
            TransferAccountInfo::new(confidential_transfer_account)
        };

        let new_decryptable_available_balance = account_info
            .new_decryptable_available_balance(transfer_amount, source_aes_key)
            .map_err(|_| TokenError::AccountDecryption)?;

        let mut instruction =
            confidential_transfer::instruction::transfer_with_fee_and_split_proofs(
                &self.program_id,
                source_account,
                self.get_address(),
                destination_account,
                new_decryptable_available_balance.into(),
                source_authority,
                context_state_accounts,
                source_decrypt_handles,
            )?;
        offchain::add_extra_account_metas(
            &mut instruction,
            source_account,
            self.get_address(),
            destination_account,
            source_authority,
            u64::MAX,
            |address| {
                self.client
                    .get_account(address)
                    .map_ok(|opt| opt.map(|acc| acc.data))
            },
        )
        .await
        .map_err(|_| TokenError::AccountNotFound)?;
        self.process_ixs(&[instruction], signing_keypairs).await
    }

    /// Transfer tokens confidentially using split proofs in parallel
    ///
    /// This function internally generates the ZK Token proof instructions to
    /// create the necessary proof context states.
    #[allow(clippy::too_many_arguments)]
    pub async fn confidential_transfer_transfer_with_fee_and_split_proofs_in_parallel<
        S: Signers,
    >(
        &self,
        source_account: &Pubkey,
        destination_account: &Pubkey,
        source_authority: &Pubkey,
        context_state_accounts: TransferWithFeeSplitContextStateAccounts<'_>,
        transfer_amount: u64,
        account_info: Option<TransferAccountInfo>,
        source_elgamal_keypair: &ElGamalKeypair,
        source_aes_key: &AeKey,
        destination_elgamal_pubkey: &ElGamalPubkey,
        auditor_elgamal_pubkey: Option<&ElGamalPubkey>,
        withdraw_withheld_authority_elgamal_pubkey: &ElGamalPubkey,
        fee_rate_basis_points: u16,
        maximum_fee: u64,
        equality_and_ciphertext_validity_proof_signers: &S,
        fee_sigma_proof_signers: &S,
        range_proof_signers: &S,
    ) -> TokenResult<(T::Output, T::Output, T::Output)> {
        let account_info = if let Some(account_info) = account_info {
            account_info
        } else {
            let account = self.get_account_info(source_account).await?;
            let confidential_transfer_account =
                account.get_extension::<ConfidentialTransferAccount>()?;
            TransferAccountInfo::new(confidential_transfer_account)
        };

        let current_source_available_balance = account_info
            .available_balance
            .try_into()
            .map_err(|_| TokenError::AccountDecryption)?;
        let current_decryptable_available_balance = account_info
            .decryptable_available_balance
            .try_into()
            .map_err(|_| TokenError::AccountDecryption)?;

        let fee_parameters = FeeParameters {
            fee_rate_basis_points,
            maximum_fee,
        };

        let (
            equality_proof_data,
            transfer_amount_ciphertext_validity_proof_data,
            fee_sigma_proof_data,
            fee_ciphertext_validity_proof_data,
            range_proof_data,
            source_decrypt_handles,
        ) = transfer_with_fee_split_proof_data(
            &current_source_available_balance,
            &current_decryptable_available_balance,
            transfer_amount,
            source_elgamal_keypair,
            source_aes_key,
            destination_elgamal_pubkey,
            auditor_elgamal_pubkey,
            withdraw_withheld_authority_elgamal_pubkey,
            &fee_parameters,
        )
        .map_err(|_| TokenError::ProofGeneration)?;

        let new_decryptable_available_balance = account_info
            .new_decryptable_available_balance(transfer_amount, source_aes_key)
            .map_err(|_| TokenError::AccountDecryption)?;

        let mut transfer_instruction =
            confidential_transfer::instruction::transfer_with_fee_and_split_proofs(
                &self.program_id,
                source_account,
                self.get_address(),
                destination_account,
                new_decryptable_available_balance.into(),
                source_authority,
                context_state_accounts,
                &source_decrypt_handles,
            )?;
        offchain::add_extra_account_metas(
            &mut transfer_instruction,
            source_account,
            self.get_address(),
            destination_account,
            source_authority,
            u64::MAX,
            |address| {
                self.client
                    .get_account(address)
                    .map_ok(|opt| opt.map(|acc| acc.data))
            },
        )
        .await
        .map_err(|_| TokenError::AccountNotFound)?;

        let transfer_with_equality_and_ciphertext_valdity = self
            .create_equality_and_ciphertext_validity_proof_context_states_for_transfer_with_fee_parallel(
                context_state_accounts,
                &equality_proof_data,
                &transfer_amount_ciphertext_validity_proof_data,
                &transfer_instruction,
                equality_and_ciphertext_validity_proof_signers
            );

        let transfer_with_fee_sigma_and_ciphertext_validity = self
            .create_fee_sigma_and_ciphertext_validity_proof_context_states_for_transfer_with_fee_parallel(
                context_state_accounts,
                &fee_sigma_proof_data,
                &fee_ciphertext_validity_proof_data,
                &transfer_instruction,
                fee_sigma_proof_signers,
            );

        let transfer_with_range_proof = self
            .create_range_proof_context_state_for_transfer_with_fee_parallel(
                context_state_accounts,
                &range_proof_data,
                &transfer_instruction,
                range_proof_signers,
            );

        try_join!(
            transfer_with_equality_and_ciphertext_valdity,
            transfer_with_fee_sigma_and_ciphertext_validity,
            transfer_with_range_proof,
        )
    }

    /// Create equality and transfer amount ciphertext validity proof context
    /// state accounts for a confidential transfer with fee.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_equality_and_ciphertext_validity_proof_context_states_for_transfer_with_fee<
        S: Signers,
    >(
        &self,
        context_state_accounts: TransferWithFeeSplitContextStateAccounts<'_>,
        equality_proof_data: &CiphertextCommitmentEqualityProofData,
        ciphertext_validity_proof_data: &BatchedGroupedCiphertext2HandlesValidityProofData,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.create_equality_and_ciphertext_validity_proof_context_states_with_optional_transfer_with_fee(
            context_state_accounts,
            equality_proof_data,
            ciphertext_validity_proof_data,
            None,
            signing_keypairs,
        )
        .await
    }

    /// Create equality and transfer amount ciphertext validity proof context
    /// state accounts with a confidential transfer instruction.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_equality_and_ciphertext_validity_proof_context_states_for_transfer_with_fee_parallel<
        S: Signers,
    >(
        &self,
        context_state_accounts: TransferWithFeeSplitContextStateAccounts<'_>,
        equality_proof_data: &CiphertextCommitmentEqualityProofData,
        ciphertext_validity_proof_data: &BatchedGroupedCiphertext2HandlesValidityProofData,
        transfer_instruction: &Instruction,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.create_equality_and_ciphertext_validity_proof_context_states_with_optional_transfer_with_fee(
            context_state_accounts,
            equality_proof_data,
            ciphertext_validity_proof_data,
            Some(transfer_instruction),
            signing_keypairs,
        )
        .await
    }

    /// Create equality and ciphertext validity proof context states for a
    /// confidential transfer with fee.
    ///
    /// If an optional transfer instruction is provided, then the transfer
    /// instruction is attached to the same transaction.
    #[allow(clippy::too_many_arguments)]
    async fn create_equality_and_ciphertext_validity_proof_context_states_with_optional_transfer_with_fee<
        S: Signers,
    >(
        &self,
        context_state_accounts: TransferWithFeeSplitContextStateAccounts<'_>,
        equality_proof_data: &CiphertextCommitmentEqualityProofData,
        transfer_amount_ciphertext_validity_proof_data: &BatchedGroupedCiphertext2HandlesValidityProofData,
        transfer_instruction: Option<&Instruction>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let mut instructions = vec![];

        // create equality proof context state
        let instruction_type = ProofInstruction::VerifyCiphertextCommitmentEquality;
        let space = size_of::<ProofContextState<CiphertextCommitmentEqualityProofContext>>();
        let rent = self
            .client
            .get_minimum_balance_for_rent_exemption(space)
            .await
            .map_err(TokenError::Client)?;
        instructions.push(system_instruction::create_account(
            &self.payer.pubkey(),
            context_state_accounts.equality_proof,
            rent,
            space as u64,
            &zk_token_proof_program::id(),
        ));

        let equality_proof_context_state_info = ContextStateInfo {
            context_state_account: context_state_accounts.equality_proof,
            context_state_authority: context_state_accounts.authority,
        };
        instructions.push(
            instruction_type
                .encode_verify_proof(Some(equality_proof_context_state_info), equality_proof_data),
        );

        // create transfer amount ciphertext validity proof context state
        let instruction_type = ProofInstruction::VerifyBatchedGroupedCiphertext2HandlesValidity;
        let space =
            size_of::<ProofContextState<BatchedGroupedCiphertext2HandlesValidityProofContext>>();
        let rent = self
            .client
            .get_minimum_balance_for_rent_exemption(space)
            .await
            .map_err(TokenError::Client)?;
        instructions.push(system_instruction::create_account(
            &self.payer.pubkey(),
            context_state_accounts.transfer_amount_ciphertext_validity_proof,
            rent,
            space as u64,
            &zk_token_proof_program::id(),
        ));

        let transfer_amount_ciphertext_validity_proof_context_state_info = ContextStateInfo {
            context_state_account: context_state_accounts.transfer_amount_ciphertext_validity_proof,
            context_state_authority: context_state_accounts.authority,
        };
        instructions.push(instruction_type.encode_verify_proof(
            Some(transfer_amount_ciphertext_validity_proof_context_state_info),
            transfer_amount_ciphertext_validity_proof_data,
        ));

        // add transfer instruction
        if let Some(transfer_instruction) = transfer_instruction {
            instructions.push(transfer_instruction.clone());
        }

        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Create fee sigma and fee ciphertext validity proof context state
    /// accounts for a confidential transfer with fee.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_fee_sigma_and_ciphertext_validity_proof_context_states_for_transfer_with_fee<
        S: Signers,
    >(
        &self,
        context_state_accounts: TransferWithFeeSplitContextStateAccounts<'_>,
        fee_sigma_proof_data: &FeeSigmaProofData,
        fee_ciphertext_validity_proof_data: &BatchedGroupedCiphertext2HandlesValidityProofData,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.create_fee_sigma_and_ciphertext_validity_proof_context_states_with_optional_transfer_with_fee(
            context_state_accounts,
            fee_sigma_proof_data,
            fee_ciphertext_validity_proof_data,
            None,
            signing_keypairs,
        )
        .await
    }

    /// Create fee sigma and fee ciphertext validity proof context state
    /// accounts with a confidential transfer with fee.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_fee_sigma_and_ciphertext_validity_proof_context_states_for_transfer_with_fee_parallel<
        S: Signers,
    >(
        &self,
        context_state_accounts: TransferWithFeeSplitContextStateAccounts<'_>,
        fee_sigma_proof_data: &FeeSigmaProofData,
        fee_ciphertext_validity_proof_data: &BatchedGroupedCiphertext2HandlesValidityProofData,
        transfer_instruction: &Instruction,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.create_fee_sigma_and_ciphertext_validity_proof_context_states_with_optional_transfer_with_fee(
            context_state_accounts,
            fee_sigma_proof_data,
            fee_ciphertext_validity_proof_data,
            Some(transfer_instruction),
            signing_keypairs,
        )
        .await
    }

    /// Create fee sigma and fee ciphertext validity proof context states for a
    /// confidential transfer with fee.
    ///
    /// If an optional transfer instruction is provided, then the transfer
    /// instruction is attached to the same transaction.
    #[allow(clippy::too_many_arguments)]
    async fn create_fee_sigma_and_ciphertext_validity_proof_context_states_with_optional_transfer_with_fee<
        S: Signers,
    >(
        &self,
        context_state_accounts: TransferWithFeeSplitContextStateAccounts<'_>,
        fee_sigma_proof_data: &FeeSigmaProofData,
        fee_ciphertext_validity_proof_data: &BatchedGroupedCiphertext2HandlesValidityProofData,
        transfer_instruction: Option<&Instruction>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let mut instructions = vec![];

        // create fee sigma proof context state
        let instruction_type = ProofInstruction::VerifyFeeSigma;
        let space = size_of::<ProofContextState<FeeSigmaProofContext>>();
        let rent = self
            .client
            .get_minimum_balance_for_rent_exemption(space)
            .await
            .map_err(TokenError::Client)?;
        instructions.push(system_instruction::create_account(
            &self.payer.pubkey(),
            context_state_accounts.fee_sigma_proof,
            rent,
            space as u64,
            &zk_token_proof_program::id(),
        ));

        let fee_sigma_proof_context_state_info = ContextStateInfo {
            context_state_account: context_state_accounts.fee_sigma_proof,
            context_state_authority: context_state_accounts.authority,
        };
        instructions.push(instruction_type.encode_verify_proof(
            Some(fee_sigma_proof_context_state_info),
            fee_sigma_proof_data,
        ));

        // create fee ciphertext validity proof context state
        let instruction_type = ProofInstruction::VerifyBatchedGroupedCiphertext2HandlesValidity;
        let space =
            size_of::<ProofContextState<BatchedGroupedCiphertext2HandlesValidityProofContext>>();
        let rent = self
            .client
            .get_minimum_balance_for_rent_exemption(space)
            .await
            .map_err(TokenError::Client)?;
        instructions.push(system_instruction::create_account(
            &self.payer.pubkey(),
            context_state_accounts.fee_ciphertext_validity_proof,
            rent,
            space as u64,
            &zk_token_proof_program::id(),
        ));

        let fee_ciphertext_validity_proof_context_state_info = ContextStateInfo {
            context_state_account: context_state_accounts.fee_ciphertext_validity_proof,
            context_state_authority: context_state_accounts.authority,
        };
        instructions.push(instruction_type.encode_verify_proof(
            Some(fee_ciphertext_validity_proof_context_state_info),
            fee_ciphertext_validity_proof_data,
        ));

        // add transfer instruction
        if let Some(transfer_instruction) = transfer_instruction {
            instructions.push(transfer_instruction.clone());
        }

        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Create range proof context state account for a confidential transfer
    /// with fee.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_range_proof_context_state_for_transfer_with_fee<S: Signers>(
        &self,
        context_state_accounts: TransferWithFeeSplitContextStateAccounts<'_>,
        range_proof_data: &BatchedRangeProofU256Data,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.create_range_proof_context_state_with_optional_transfer_with_fee(
            context_state_accounts,
            range_proof_data,
            None,
            signing_keypairs,
        )
        .await
    }

    /// Create range proof context state account for a confidential transfer
    /// with fee.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_range_proof_context_state_for_transfer_with_fee_parallel<S: Signers>(
        &self,
        context_state_accounts: TransferWithFeeSplitContextStateAccounts<'_>,
        range_proof_data: &BatchedRangeProofU256Data,
        transfer_instruction: &Instruction,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.create_range_proof_context_state_with_optional_transfer_with_fee(
            context_state_accounts,
            range_proof_data,
            Some(transfer_instruction),
            signing_keypairs,
        )
        .await
    }

    /// Create a range proof context state account and an optional confidential
    /// transfer instruction.
    async fn create_range_proof_context_state_with_optional_transfer_with_fee<S: Signers>(
        &self,
        context_state_accounts: TransferWithFeeSplitContextStateAccounts<'_>,
        range_proof_data: &BatchedRangeProofU256Data,
        transfer_instruction: Option<&Instruction>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let instruction_type = ProofInstruction::VerifyBatchedRangeProofU256;
        let space = size_of::<ProofContextState<BatchedRangeProofContext>>();
        let rent = self
            .client
            .get_minimum_balance_for_rent_exemption(space)
            .await
            .map_err(TokenError::Client)?;
        let range_proof_context_state_info = ContextStateInfo {
            context_state_account: context_state_accounts.range_proof,
            context_state_authority: context_state_accounts.authority,
        };

        let mut instructions = vec![
            system_instruction::create_account(
                &self.payer.pubkey(),
                context_state_accounts.range_proof,
                rent,
                space as u64,
                &zk_token_proof_program::id(),
            ),
            instruction_type
                .encode_verify_proof(Some(range_proof_context_state_info), range_proof_data),
        ];

        if let Some(transfer_instruction) = transfer_instruction {
            instructions.push(transfer_instruction.clone());
        }

        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Applies the confidential transfer pending balance to the available
    /// balance
    pub async fn confidential_transfer_apply_pending_balance<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        account_info: Option<ApplyPendingBalanceAccountInfo>,
        elgamal_secret_key: &ElGamalSecretKey,
        aes_key: &AeKey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        let account_info = if let Some(account_info) = account_info {
            account_info
        } else {
            let account = self.get_account_info(account).await?;
            let confidential_transfer_account =
                account.get_extension::<ConfidentialTransferAccount>()?;
            ApplyPendingBalanceAccountInfo::new(confidential_transfer_account)
        };

        let expected_pending_balance_credit_counter = account_info.pending_balance_credit_counter();
        let new_decryptable_available_balance = account_info
            .new_decryptable_available_balance(elgamal_secret_key, aes_key)
            .map_err(|_| TokenError::AccountDecryption)?;

        self.process_ixs(
            &[confidential_transfer::instruction::apply_pending_balance(
                &self.program_id,
                account,
                expected_pending_balance_credit_counter,
                new_decryptable_available_balance,
                authority,
                &multisig_signers,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Enable confidential transfer `Deposit` and `Transfer` instructions for a
    /// token account
    pub async fn confidential_transfer_enable_confidential_credits<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[
                confidential_transfer::instruction::enable_confidential_credits(
                    &self.program_id,
                    account,
                    authority,
                    &multisig_signers,
                )?,
            ],
            signing_keypairs,
        )
        .await
    }

    /// Disable confidential transfer `Deposit` and `Transfer` instructions for
    /// a token account
    pub async fn confidential_transfer_disable_confidential_credits<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[
                confidential_transfer::instruction::disable_confidential_credits(
                    &self.program_id,
                    account,
                    authority,
                    &multisig_signers,
                )?,
            ],
            signing_keypairs,
        )
        .await
    }

    /// Enable a confidential extension token account to receive
    /// non-confidential payments
    pub async fn confidential_transfer_enable_non_confidential_credits<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[
                confidential_transfer::instruction::enable_non_confidential_credits(
                    &self.program_id,
                    account,
                    authority,
                    &multisig_signers,
                )?,
            ],
            signing_keypairs,
        )
        .await
    }

    /// Disable non-confidential payments for a confidential extension token
    /// account
    pub async fn confidential_transfer_disable_non_confidential_credits<S: Signers>(
        &self,
        account: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[
                confidential_transfer::instruction::disable_non_confidential_credits(
                    &self.program_id,
                    account,
                    authority,
                    &multisig_signers,
                )?,
            ],
            signing_keypairs,
        )
        .await
    }

    /// Withdraw withheld confidential tokens from mint
    #[allow(clippy::too_many_arguments)]
    pub async fn confidential_transfer_withdraw_withheld_tokens_from_mint<S: Signers>(
        &self,
        destination_account: &Pubkey,
        withdraw_withheld_authority: &Pubkey,
        context_state_account: Option<&Pubkey>,
        withheld_tokens_info: Option<WithheldTokensInfo>,
        withdraw_withheld_authority_elgamal_keypair: &ElGamalKeypair,
        destination_elgamal_pubkey: &ElGamalPubkey,
        new_decryptable_available_balance: &DecryptableBalance,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers =
            self.get_multisig_signers(withdraw_withheld_authority, &signing_pubkeys);

        let account_info = if let Some(account_info) = withheld_tokens_info {
            account_info
        } else {
            let mint_info = self.get_mint_info().await?;
            let confidential_transfer_fee_config =
                mint_info.get_extension::<ConfidentialTransferFeeConfig>()?;
            WithheldTokensInfo::new(&confidential_transfer_fee_config.withheld_amount)
        };

        let proof_data = if context_state_account.is_some() {
            None
        } else {
            Some(
                account_info
                    .generate_proof_data(
                        withdraw_withheld_authority_elgamal_keypair,
                        destination_elgamal_pubkey,
                    )
                    .map_err(|_| TokenError::ProofGeneration)?,
            )
        };

        let proof_location = if let Some(proof_data_temp) = proof_data.as_ref() {
            ProofLocation::InstructionOffset(1.try_into().unwrap(), proof_data_temp)
        } else {
            let context_state_account = context_state_account.unwrap();
            ProofLocation::ContextStateAccount(context_state_account)
        };

        self.process_ixs(
            &confidential_transfer_fee::instruction::withdraw_withheld_tokens_from_mint(
                &self.program_id,
                &self.pubkey,
                destination_account,
                new_decryptable_available_balance,
                withdraw_withheld_authority,
                &multisig_signers,
                proof_location,
            )?,
            signing_keypairs,
        )
        .await
    }

    /// Withdraw withheld confidential tokens from accounts
    #[allow(clippy::too_many_arguments)]
    pub async fn confidential_transfer_withdraw_withheld_tokens_from_accounts<S: Signers>(
        &self,
        destination_account: &Pubkey,
        withdraw_withheld_authority: &Pubkey,
        context_state_account: Option<&Pubkey>,
        withheld_tokens_info: Option<WithheldTokensInfo>,
        withdraw_withheld_authority_elgamal_keypair: &ElGamalKeypair,
        destination_elgamal_pubkey: &ElGamalPubkey,
        new_decryptable_available_balance: &DecryptableBalance,
        sources: &[&Pubkey],
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers =
            self.get_multisig_signers(withdraw_withheld_authority, &signing_pubkeys);

        let account_info = if let Some(account_info) = withheld_tokens_info {
            account_info
        } else {
            let futures = sources.iter().map(|source| self.get_account_info(source));
            let sources_extensions = join_all(futures).await;

            let mut aggregate_withheld_amount = ElGamalCiphertext::default();
            for source_extension in sources_extensions {
                let withheld_amount: ElGamalCiphertext = source_extension?
                    .get_extension::<ConfidentialTransferFeeAmount>()?
                    .withheld_amount
                    .try_into()
                    .map_err(|_| TokenError::AccountDecryption)?;
                aggregate_withheld_amount = aggregate_withheld_amount + withheld_amount;
            }

            WithheldTokensInfo::new(&aggregate_withheld_amount.into())
        };

        let proof_data = if context_state_account.is_some() {
            None
        } else {
            Some(
                account_info
                    .generate_proof_data(
                        withdraw_withheld_authority_elgamal_keypair,
                        destination_elgamal_pubkey,
                    )
                    .map_err(|_| TokenError::ProofGeneration)?,
            )
        };

        let proof_location = if let Some(proof_data_temp) = proof_data.as_ref() {
            ProofLocation::InstructionOffset(1.try_into().unwrap(), proof_data_temp)
        } else {
            let context_state_account = context_state_account.unwrap();
            ProofLocation::ContextStateAccount(context_state_account)
        };

        self.process_ixs(
            &confidential_transfer_fee::instruction::withdraw_withheld_tokens_from_accounts(
                &self.program_id,
                &self.pubkey,
                destination_account,
                new_decryptable_available_balance,
                withdraw_withheld_authority,
                &multisig_signers,
                sources,
                proof_location,
            )?,
            signing_keypairs,
        )
        .await
    }

    /// Harvest withheld confidential tokens to mint
    pub async fn confidential_transfer_harvest_withheld_tokens_to_mint(
        &self,
        sources: &[&Pubkey],
    ) -> TokenResult<T::Output> {
        self.process_ixs::<[&dyn Signer; 0]>(
            &[
                confidential_transfer_fee::instruction::harvest_withheld_tokens_to_mint(
                    &self.program_id,
                    &self.pubkey,
                    sources,
                )?,
            ],
            &[],
        )
        .await
    }

    /// Enable harvest of confidential fees to mint
    pub async fn confidential_transfer_enable_harvest_to_mint<S: Signers>(
        &self,
        withdraw_withheld_authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers =
            self.get_multisig_signers(withdraw_withheld_authority, &signing_pubkeys);

        self.process_ixs(
            &[
                confidential_transfer_fee::instruction::enable_harvest_to_mint(
                    &self.program_id,
                    &self.pubkey,
                    withdraw_withheld_authority,
                    &multisig_signers,
                )?,
            ],
            signing_keypairs,
        )
        .await
    }

    /// Disable harvest of confidential fees to mint
    pub async fn confidential_transfer_disable_harvest_to_mint<S: Signers>(
        &self,
        withdraw_withheld_authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers =
            self.get_multisig_signers(withdraw_withheld_authority, &signing_pubkeys);

        self.process_ixs(
            &[
                confidential_transfer_fee::instruction::disable_harvest_to_mint(
                    &self.program_id,
                    &self.pubkey,
                    withdraw_withheld_authority,
                    &multisig_signers,
                )?,
            ],
            signing_keypairs,
        )
        .await
    }

    pub async fn withdraw_excess_lamports<S: Signers>(
        &self,
        source: &Pubkey,
        destination: &Pubkey,
        authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let signing_pubkeys = signing_keypairs.pubkeys();
        let multisig_signers = self.get_multisig_signers(authority, &signing_pubkeys);

        self.process_ixs(
            &[spl_token_2022::instruction::withdraw_excess_lamports(
                &self.program_id,
                source,
                destination,
                authority,
                &multisig_signers,
            )?],
            signing_keypairs,
        )
        .await
    }

    /// Initialize token-metadata on a mint
    pub async fn token_metadata_initialize<S: Signers>(
        &self,
        update_authority: &Pubkey,
        mint_authority: &Pubkey,
        name: String,
        symbol: String,
        uri: String,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.process_ixs(
            &[spl_token_metadata_interface::instruction::initialize(
                &self.program_id,
                &self.pubkey,
                update_authority,
                &self.pubkey,
                mint_authority,
                name,
                symbol,
                uri,
            )],
            signing_keypairs,
        )
        .await
    }

    async fn get_additional_rent_for_new_metadata(
        &self,
        token_metadata: &TokenMetadata,
    ) -> TokenResult<u64> {
        let account = self.get_account(self.pubkey).await?;
        let account_lamports = account.lamports;
        let mint_state = self.unpack_mint_info(account)?;
        let new_account_len = mint_state
            .try_get_new_account_len_for_variable_len_extension::<TokenMetadata>(token_metadata)?;
        let new_rent_exempt_minimum = self
            .client
            .get_minimum_balance_for_rent_exemption(new_account_len)
            .await
            .map_err(TokenError::Client)?;
        Ok(new_rent_exempt_minimum.saturating_sub(account_lamports))
    }

    /// Initialize token-metadata on a mint
    #[allow(clippy::too_many_arguments)]
    pub async fn token_metadata_initialize_with_rent_transfer<S: Signers>(
        &self,
        payer: &Pubkey,
        update_authority: &Pubkey,
        mint_authority: &Pubkey,
        name: String,
        symbol: String,
        uri: String,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let token_metadata = TokenMetadata {
            name,
            symbol,
            uri,
            ..Default::default()
        };
        let additional_lamports = self
            .get_additional_rent_for_new_metadata(&token_metadata)
            .await?;
        let mut instructions = vec![];
        if additional_lamports > 0 {
            instructions.push(system_instruction::transfer(
                payer,
                &self.pubkey,
                additional_lamports,
            ));
        }
        instructions.push(spl_token_metadata_interface::instruction::initialize(
            &self.program_id,
            &self.pubkey,
            update_authority,
            &self.pubkey,
            mint_authority,
            token_metadata.name,
            token_metadata.symbol,
            token_metadata.uri,
        ));
        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Update a token-metadata field on a mint
    pub async fn token_metadata_update_field<S: Signers>(
        &self,
        update_authority: &Pubkey,
        field: Field,
        value: String,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.process_ixs(
            &[spl_token_metadata_interface::instruction::update_field(
                &self.program_id,
                &self.pubkey,
                update_authority,
                field,
                value,
            )],
            signing_keypairs,
        )
        .await
    }

    async fn get_additional_rent_for_updated_metadata(
        &self,
        field: Field,
        value: String,
    ) -> TokenResult<u64> {
        let account = self.get_account(self.pubkey).await?;
        let account_lamports = account.lamports;
        let mint_state = self.unpack_mint_info(account)?;
        let mut token_metadata = mint_state.get_variable_len_extension::<TokenMetadata>()?;
        token_metadata.update(field, value);
        let new_account_len = mint_state
            .try_get_new_account_len_for_variable_len_extension::<TokenMetadata>(&token_metadata)?;
        let new_rent_exempt_minimum = self
            .client
            .get_minimum_balance_for_rent_exemption(new_account_len)
            .await
            .map_err(TokenError::Client)?;
        Ok(new_rent_exempt_minimum.saturating_sub(account_lamports))
    }

    /// Update a token-metadata field on a mint. Includes a transfer for any
    /// additional rent-exempt SOL required.
    #[allow(clippy::too_many_arguments)]
    pub async fn token_metadata_update_field_with_rent_transfer<S: Signers>(
        &self,
        payer: &Pubkey,
        update_authority: &Pubkey,
        field: Field,
        value: String,
        transfer_lamports: Option<u64>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let additional_lamports = if let Some(transfer_lamports) = transfer_lamports {
            transfer_lamports
        } else {
            self.get_additional_rent_for_updated_metadata(field.clone(), value.clone())
                .await?
        };
        let mut instructions = vec![];
        if additional_lamports > 0 {
            instructions.push(system_instruction::transfer(
                payer,
                &self.pubkey,
                additional_lamports,
            ));
        }
        instructions.push(spl_token_metadata_interface::instruction::update_field(
            &self.program_id,
            &self.pubkey,
            update_authority,
            field,
            value,
        ));
        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Update the token-metadata authority in a mint
    pub async fn token_metadata_update_authority<S: Signers>(
        &self,
        current_authority: &Pubkey,
        new_authority: Option<Pubkey>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.process_ixs(
            &[spl_token_metadata_interface::instruction::update_authority(
                &self.program_id,
                &self.pubkey,
                current_authority,
                new_authority.try_into()?,
            )],
            signing_keypairs,
        )
        .await
    }

    /// Remove a token-metadata field on a mint
    pub async fn token_metadata_remove_key<S: Signers>(
        &self,
        update_authority: &Pubkey,
        key: String,
        idempotent: bool,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.process_ixs(
            &[spl_token_metadata_interface::instruction::remove_key(
                &self.program_id,
                &self.pubkey,
                update_authority,
                key,
                idempotent,
            )],
            signing_keypairs,
        )
        .await
    }

    /// Initialize token-group on a mint
    pub async fn token_group_initialize<S: Signers>(
        &self,
        mint_authority: &Pubkey,
        update_authority: &Pubkey,
        max_size: u32,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.process_ixs(
            &[spl_token_group_interface::instruction::initialize_group(
                &self.program_id,
                &self.pubkey,
                &self.pubkey,
                mint_authority,
                Some(*update_authority),
                max_size,
            )],
            signing_keypairs,
        )
        .await
    }

    async fn get_additional_rent_for_fixed_len_extension<V: Extension + Pod>(
        &self,
    ) -> TokenResult<u64> {
        let account = self.get_account(self.pubkey).await?;
        let account_lamports = account.lamports;
        let mint_state = self.unpack_mint_info(account)?;
        if mint_state.get_extension::<V>().is_ok() {
            Ok(0)
        } else {
            let new_account_len = mint_state.try_get_new_account_len::<V>()?;
            let new_rent_exempt_minimum = self
                .client
                .get_minimum_balance_for_rent_exemption(new_account_len)
                .await
                .map_err(TokenError::Client)?;
            Ok(new_rent_exempt_minimum.saturating_sub(account_lamports))
        }
    }

    /// Initialize token-group on a mint
    pub async fn token_group_initialize_with_rent_transfer<S: Signers>(
        &self,
        payer: &Pubkey,
        mint_authority: &Pubkey,
        update_authority: &Pubkey,
        max_size: u32,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let additional_lamports = self
            .get_additional_rent_for_fixed_len_extension::<TokenGroup>()
            .await?;
        let mut instructions = vec![];
        if additional_lamports > 0 {
            instructions.push(system_instruction::transfer(
                payer,
                &self.pubkey,
                additional_lamports,
            ));
        }
        instructions.push(spl_token_group_interface::instruction::initialize_group(
            &self.program_id,
            &self.pubkey,
            &self.pubkey,
            mint_authority,
            Some(*update_authority),
            max_size,
        ));
        self.process_ixs(&instructions, signing_keypairs).await
    }

    /// Update a token-group max size on a mint
    pub async fn token_group_update_max_size<S: Signers>(
        &self,
        update_authority: &Pubkey,
        new_max_size: u32,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.process_ixs(
            &[
                spl_token_group_interface::instruction::update_group_max_size(
                    &self.program_id,
                    &self.pubkey,
                    update_authority,
                    new_max_size,
                ),
            ],
            signing_keypairs,
        )
        .await
    }

    /// Update the token-group authority in a mint
    pub async fn token_group_update_authority<S: Signers>(
        &self,
        current_authority: &Pubkey,
        new_authority: Option<Pubkey>,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.process_ixs(
            &[
                spl_token_group_interface::instruction::update_group_authority(
                    &self.program_id,
                    &self.pubkey,
                    current_authority,
                    new_authority,
                ),
            ],
            signing_keypairs,
        )
        .await
    }

    /// Initialize a token-group member on a mint
    pub async fn token_group_initialize_member<S: Signers>(
        &self,
        mint_authority: &Pubkey,
        group_mint: &Pubkey,
        group_update_authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        self.process_ixs(
            &[spl_token_group_interface::instruction::initialize_member(
                &self.program_id,
                &self.pubkey,
                &self.pubkey,
                mint_authority,
                group_mint,
                group_update_authority,
            )],
            signing_keypairs,
        )
        .await
    }

    /// Initialize a token-group member on a mint
    #[allow(clippy::too_many_arguments)]
    pub async fn token_group_initialize_member_with_rent_transfer<S: Signers>(
        &self,
        payer: &Pubkey,
        mint_authority: &Pubkey,
        group_mint: &Pubkey,
        group_update_authority: &Pubkey,
        signing_keypairs: &S,
    ) -> TokenResult<T::Output> {
        let additional_lamports = self
            .get_additional_rent_for_fixed_len_extension::<TokenGroupMember>()
            .await?;
        let mut instructions = vec![];
        if additional_lamports > 0 {
            instructions.push(system_instruction::transfer(
                payer,
                &self.pubkey,
                additional_lamports,
            ));
        }
        instructions.push(spl_token_group_interface::instruction::initialize_member(
            &self.program_id,
            &self.pubkey,
            &self.pubkey,
            mint_authority,
            group_mint,
            group_update_authority,
        ));
        self.process_ixs(&instructions, signing_keypairs).await
    }
}
