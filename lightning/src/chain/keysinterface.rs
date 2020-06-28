//! keysinterface provides keys into rust-lightning and defines some useful enums which describe
//! spendable on-chain outputs which the user owns and is responsible for using just as any other
//! on-chain output which is theirs.

use bitcoin::blockdata::transaction::{Transaction, OutPoint, TxOut};
use bitcoin::blockdata::script::{Script, Builder};
use bitcoin::blockdata::opcodes;
use bitcoin::network::constants::Network;
use bitcoin::util::bip32::{ExtendedPrivKey, ExtendedPubKey, ChildNumber};
use bitcoin::util::bip143;

use bitcoin::hashes::{Hash, HashEngine};
use bitcoin::hashes::sha256::HashEngine as Sha256State;
use bitcoin::hashes::sha256::Hash as Sha256;
use bitcoin::hashes::sha256d::Hash as Sha256dHash;
use bitcoin::hash_types::WPubkeyHash;

use bitcoin::secp256k1::key::{SecretKey, PublicKey};
use bitcoin::secp256k1::{Secp256k1, Signature, Signing};
use bitcoin::secp256k1;

use util::byte_utils;
use util::ser::{Writeable, Writer, Readable};

use ln::chan_utils;
use ln::chan_utils::{TxCreationKeys, HTLCOutputInCommitment, make_funding_redeemscript, ChannelPublicKeys, LocalCommitmentTransaction};
use ln::msgs;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::io::Error;
use ln::msgs::DecodeError;

/// When on-chain outputs are created by rust-lightning (which our counterparty is not able to
/// claim at any point in the future) an event is generated which you must track and be able to
/// spend on-chain. The information needed to do this is provided in this enum, including the
/// outpoint describing which txid and output index is available, the full output which exists at
/// that txid/index, and any keys or other information required to sign.
#[derive(Clone, PartialEq)]
pub enum SpendableOutputDescriptor {
	/// An output to a script which was provided via KeysInterface, thus you should already know
	/// how to spend it. No keys are provided as rust-lightning was never given any keys - only the
	/// script_pubkey as it appears in the output.
	/// These may include outputs from a transaction punishing our counterparty or claiming an HTLC
	/// on-chain using the payment preimage or after it has timed out.
	StaticOutput {
		/// The outpoint which is spendable
		outpoint: OutPoint,
		/// The output which is referenced by the given outpoint.
		output: TxOut,
	},
	/// An output to a P2WSH script which can be spent with a single signature after a CSV delay.
	///
	/// The witness in the spending input should be:
	/// <BIP 143 signature> <empty vector> (MINIMALIF standard rule) <provided witnessScript>
	///
	/// Note that the nSequence field in the spending input must be set to to_self_delay
	/// (which means the transaction is not broadcastable until at least to_self_delay
	/// blocks after the outpoint confirms).
	///
	/// These are generally the result of a "revocable" output to us, spendable only by us unless
	/// it is an output from an old state which we broadcast (which should never happen).
	///
	/// To derive the delayed_payment key which is used to sign for this input, you must pass the
	/// local delayed_payment_base_key (ie the private key which corresponds to the pubkey in
	/// ChannelKeys::pubkeys().delayed_payment_basepoint) and the provided per_commitment_point to
	/// chan_utils::derive_private_key. The public key can be generated without the secret key
	/// using chan_utils::derive_public_key and only the delayed_payment_basepoint which appears in
	/// ChannelKeys::pubkeys().
	///
	/// To derive the remote_revocation_pubkey provided here (which is used in the witness
	/// script generation), you must pass the remote revocation_basepoint (which appears in the
	/// call to ChannelKeys::set_remote_channel_pubkeys) and the provided per_commitment point
	/// to chan_utils::derive_public_revocation_key.
	///
	/// The witness script which is hashed and included in the output script_pubkey may be
	/// regenerated by passing the revocation_pubkey (derived as above), our delayed_payment pubkey
	/// (derived as above), and the to_self_delay contained here to
	/// chan_utils::get_revokeable_redeemscript.
	//
	// TODO: we need to expose utility methods in KeyManager to do all the relevant derivation.
	DynamicOutputP2WSH {
		/// The outpoint which is spendable
		outpoint: OutPoint,
		/// Per commitment point to derive delayed_payment_key by key holder
		per_commitment_point: PublicKey,
		/// The nSequence value which must be set in the spending input to satisfy the OP_CSV in
		/// the witness_script.
		to_self_delay: u16,
		/// The output which is referenced by the given outpoint
		output: TxOut,
		/// The channel keys state used to proceed to derivation of signing key. Must
		/// be pass to KeysInterface::derive_channel_keys.
		key_derivation_params: (u64, u64),
		/// The remote_revocation_pubkey used to derive witnessScript
		remote_revocation_pubkey: PublicKey
	},
	/// An output to a P2WPKH, spendable exclusively by our payment key (ie the private key which
	/// corresponds to the public key in ChannelKeys::pubkeys().payment_point).
	/// The witness in the spending input, is, thus, simply:
	/// <BIP 143 signature> <payment key>
	///
	/// These are generally the result of our counterparty having broadcast the current state,
	/// allowing us to claim the non-HTLC-encumbered outputs immediately.
	StaticOutputRemotePayment {
		/// The outpoint which is spendable
		outpoint: OutPoint,
		/// The output which is reference by the given outpoint
		output: TxOut,
		/// The channel keys state used to proceed to derivation of signing key. Must
		/// be pass to KeysInterface::derive_channel_keys.
		key_derivation_params: (u64, u64),
	}
}

impl Writeable for SpendableOutputDescriptor {
	fn write<W: Writer>(&self, writer: &mut W) -> Result<(), ::std::io::Error> {
		match self {
			&SpendableOutputDescriptor::StaticOutput { ref outpoint, ref output } => {
				0u8.write(writer)?;
				outpoint.write(writer)?;
				output.write(writer)?;
			},
			&SpendableOutputDescriptor::DynamicOutputP2WSH { ref outpoint, ref per_commitment_point, ref to_self_delay, ref output, ref key_derivation_params, ref remote_revocation_pubkey } => {
				1u8.write(writer)?;
				outpoint.write(writer)?;
				per_commitment_point.write(writer)?;
				to_self_delay.write(writer)?;
				output.write(writer)?;
				key_derivation_params.0.write(writer)?;
				key_derivation_params.1.write(writer)?;
				remote_revocation_pubkey.write(writer)?;
			},
			&SpendableOutputDescriptor::StaticOutputRemotePayment { ref outpoint, ref output, ref key_derivation_params } => {
				2u8.write(writer)?;
				outpoint.write(writer)?;
				output.write(writer)?;
				key_derivation_params.0.write(writer)?;
				key_derivation_params.1.write(writer)?;
			},
		}
		Ok(())
	}
}

impl Readable for SpendableOutputDescriptor {
	fn read<R: ::std::io::Read>(reader: &mut R) -> Result<Self, DecodeError> {
		match Readable::read(reader)? {
			0u8 => Ok(SpendableOutputDescriptor::StaticOutput {
				outpoint: Readable::read(reader)?,
				output: Readable::read(reader)?,
			}),
			1u8 => Ok(SpendableOutputDescriptor::DynamicOutputP2WSH {
				outpoint: Readable::read(reader)?,
				per_commitment_point: Readable::read(reader)?,
				to_self_delay: Readable::read(reader)?,
				output: Readable::read(reader)?,
				key_derivation_params: (Readable::read(reader)?, Readable::read(reader)?),
				remote_revocation_pubkey: Readable::read(reader)?,
			}),
			2u8 => Ok(SpendableOutputDescriptor::StaticOutputRemotePayment {
				outpoint: Readable::read(reader)?,
				output: Readable::read(reader)?,
				key_derivation_params: (Readable::read(reader)?, Readable::read(reader)?),
			}),
			_ => Err(DecodeError::InvalidValue),
		}
	}
}

/// Set of lightning keys needed to operate a channel as described in BOLT 3.
///
/// Signing services could be implemented on a hardware wallet. In this case,
/// the current ChannelKeys would be a front-end on top of a communication
/// channel connected to your secure device and lightning key material wouldn't
/// reside on a hot server. Nevertheless, a this deployment would still need
/// to trust the ChannelManager to avoid loss of funds as this latest component
/// could ask to sign commitment transaction with HTLCs paying to attacker pubkeys.
///
/// A more secure iteration would be to use hashlock (or payment points) to pair
/// invoice/incoming HTLCs with outgoing HTLCs to implement a no-trust-ChannelManager
/// at the price of more state and computation on the hardware wallet side. In the future,
/// we are looking forward to design such interface.
///
/// In any case, ChannelMonitor or fallback watchtowers are always going to be trusted
/// to act, as liveness and breach reply correctness are always going to be hard requirements
/// of LN security model, orthogonal of key management issues.
///
/// If you're implementing a custom signer, you almost certainly want to implement
/// Readable/Writable to serialize out a unique reference to this set of keys so
/// that you can serialize the full ChannelManager object.
///
// (TODO: We shouldn't require that, and should have an API to get them at deser time, due mostly
// to the possibility of reentrancy issues by calling the user's code during our deserialization
// routine).
// TODO: We should remove Clone by instead requesting a new ChannelKeys copy when we create
// ChannelMonitors instead of expecting to clone the one out of the Channel into the monitors.
pub trait ChannelKeys : Send+Clone {
	/// Gets the commitment seed
	fn commitment_seed(&self) -> &[u8; 32];
	/// Gets the local channel public keys and basepoints
	fn pubkeys(&self) -> &ChannelPublicKeys;
	/// Gets arbitrary identifiers describing the set of keys which are provided back to you in
	/// some SpendableOutputDescriptor types. These should be sufficient to identify this
	/// ChannelKeys object uniquely and lookup or re-derive its keys.
	fn key_derivation_params(&self) -> (u64, u64);

	/// Create a signature for a remote commitment transaction and associated HTLC transactions.
	///
	/// Note that if signing fails or is rejected, the channel will be force-closed.
	//
	// TODO: Document the things someone using this interface should enforce before signing.
	// TODO: Add more input vars to enable better checking (preferably removing commitment_tx and
	// making the callee generate it via some util function we expose)!
	fn sign_remote_commitment<T: secp256k1::Signing + secp256k1::Verification>(&self, feerate_per_kw: u32, commitment_tx: &Transaction, keys: &TxCreationKeys, htlcs: &[&HTLCOutputInCommitment], to_self_delay: u16, secp_ctx: &Secp256k1<T>) -> Result<(Signature, Vec<Signature>), ()>;

	/// Create a signature for a local commitment transaction. This will only ever be called with
	/// the same local_commitment_tx (or a copy thereof), though there are currently no guarantees
	/// that it will not be called multiple times.
	//
	// TODO: Document the things someone using this interface should enforce before signing.
	// TODO: Add more input vars to enable better checking (preferably removing commitment_tx and
	fn sign_local_commitment<T: secp256k1::Signing + secp256k1::Verification>(&self, local_commitment_tx: &LocalCommitmentTransaction, secp_ctx: &Secp256k1<T>) -> Result<Signature, ()>;

	/// Same as sign_local_commitment, but exists only for tests to get access to local commitment
	/// transactions which will be broadcasted later, after the channel has moved on to a newer
	/// state. Thus, needs its own method as sign_local_commitment may enforce that we only ever
	/// get called once.
	#[cfg(test)]
	fn unsafe_sign_local_commitment<T: secp256k1::Signing + secp256k1::Verification>(&self, local_commitment_tx: &LocalCommitmentTransaction, secp_ctx: &Secp256k1<T>) -> Result<Signature, ()>;

	/// Create a signature for each HTLC transaction spending a local commitment transaction.
	///
	/// Unlike sign_local_commitment, this may be called multiple times with *different*
	/// local_commitment_tx values. While this will never be called with a revoked
	/// local_commitment_tx, it is possible that it is called with the second-latest
	/// local_commitment_tx (only if we haven't yet revoked it) if some watchtower/secondary
	/// ChannelMonitor decided to broadcast before it had been updated to the latest.
	///
	/// Either an Err should be returned, or a Vec with one entry for each HTLC which exists in
	/// local_commitment_tx. For those HTLCs which have transaction_output_index set to None
	/// (implying they were considered dust at the time the commitment transaction was negotiated),
	/// a corresponding None should be included in the return value. All other positions in the
	/// return value must contain a signature.
	fn sign_local_commitment_htlc_transactions<T: secp256k1::Signing + secp256k1::Verification>(&self, local_commitment_tx: &LocalCommitmentTransaction, local_csv: u16, secp_ctx: &Secp256k1<T>) -> Result<Vec<Option<Signature>>, ()>;

	/// Create a signature for the given input in a transaction spending an HTLC or commitment
	/// transaction output when our counterparty broadcasts an old state.
	///
	/// A justice transaction may claim multiples outputs at the same time if timelocks are
	/// similar, but only a signature for the input at index `input` should be signed for here.
	/// It may be called multiples time for same output(s) if a fee-bump is needed with regards
	/// to an upcoming timelock expiration.
	///
	/// Amount is value of the output spent by this input, committed to in the BIP 143 signature.
	///
	/// per_commitment_key is revocation secret which was provided by our counterparty when they
	/// revoked the state which they eventually broadcast. It's not a _local_ secret key and does
	/// not allow the spending of any funds by itself (you need our local revocation_secret to do
	/// so).
	///
	/// htlc holds HTLC elements (hash, timelock) if the output being spent is a HTLC output, thus
	/// changing the format of the witness script (which is committed to in the BIP 143
	/// signatures).
	///
	/// on_remote_tx_csv is the relative lock-time that that our counterparty would have to set on
	/// their transaction were they to spend the same output. It is included in the witness script
	/// and thus committed to in the BIP 143 signature.
	fn sign_justice_transaction<T: secp256k1::Signing + secp256k1::Verification>(&self, justice_tx: &Transaction, input: usize, amount: u64, per_commitment_key: &SecretKey, htlc: &Option<HTLCOutputInCommitment>, on_remote_tx_csv: u16, secp_ctx: &Secp256k1<T>) -> Result<Signature, ()>;

	/// Create a signature for a claiming transaction for a HTLC output on a remote commitment
	/// transaction, either offered or received.
	///
	/// Such a transaction may claim multiples offered outputs at same time if we know the
	/// preimage for each when we create it, but only the input at index `input` should be
	/// signed for here. It may be called multiple times for same output(s) if a fee-bump is
	/// needed with regards to an upcoming timelock expiration.
	///
	/// Witness_script is either a offered or received script as defined in BOLT3 for HTLC
	/// outputs.
	///
	/// Amount is value of the output spent by this input, committed to in the BIP 143 signature.
	///
	/// Per_commitment_point is the dynamic point corresponding to the channel state
	/// detected onchain. It has been generated by our counterparty and is used to derive
	/// channel state keys, which are then included in the witness script and committed to in the
	/// BIP 143 signature.
	fn sign_remote_htlc_transaction<T: secp256k1::Signing + secp256k1::Verification>(&self, htlc_tx: &Transaction, input: usize, amount: u64, per_commitment_point: &PublicKey, htlc: &HTLCOutputInCommitment, secp_ctx: &Secp256k1<T>) -> Result<Signature, ()>;

	/// Create a signature for a (proposed) closing transaction.
	///
	/// Note that, due to rounding, there may be one "missing" satoshi, and either party may have
	/// chosen to forgo their output as dust.
	fn sign_closing_transaction<T: secp256k1::Signing>(&self, closing_tx: &Transaction, secp_ctx: &Secp256k1<T>) -> Result<Signature, ()>;

	/// Signs a channel announcement message with our funding key, proving it comes from one
	/// of the channel participants.
	///
	/// Note that if this fails or is rejected, the channel will not be publicly announced and
	/// our counterparty may (though likely will not) close the channel on us for violating the
	/// protocol.
	fn sign_channel_announcement<T: secp256k1::Signing>(&self, msg: &msgs::UnsignedChannelAnnouncement, secp_ctx: &Secp256k1<T>) -> Result<Signature, ()>;

	/// Set the remote channel basepoints.  This is done immediately on incoming channels
	/// and as soon as the channel is accepted on outgoing channels.
	///
	/// Will be called before any signatures are applied.
	fn set_remote_channel_pubkeys(&mut self, channel_points: &ChannelPublicKeys);
}

/// A trait to describe an object which can get user secrets and key material.
pub trait KeysInterface: Send + Sync {
	/// A type which implements ChannelKeys which will be returned by get_channel_keys.
	type ChanKeySigner : ChannelKeys;

	/// Get node secret key (aka node_id or network_key)
	fn get_node_secret(&self) -> SecretKey;
	/// Get destination redeemScript to encumber static protocol exit points.
	fn get_destination_script(&self) -> Script;
	/// Get shutdown_pubkey to use as PublicKey at channel closure
	fn get_shutdown_pubkey(&self) -> PublicKey;
	/// Get a new set of ChannelKeys for per-channel secrets. These MUST be unique even if you
	/// restarted with some stale data!
	fn get_channel_keys(&self, inbound: bool, channel_value_satoshis: u64) -> Self::ChanKeySigner;
	/// Get a secret and PRNG seed for constructing an onion packet
	fn get_onion_rand(&self) -> (SecretKey, [u8; 32]);
	/// Get a unique temporary channel id. Channels will be referred to by this until the funding
	/// transaction is created, at which point they will use the outpoint in the funding
	/// transaction.
	fn get_channel_id(&self) -> [u8; 32];
}

#[derive(Clone)]
/// A simple implementation of ChannelKeys that just keeps the private keys in memory.
pub struct InMemoryChannelKeys {
	/// Private key of anchor tx
	pub funding_key: SecretKey,
	/// Local secret key for blinded revocation pubkey
	pub revocation_base_key: SecretKey,
	/// Local secret key used for our balance in remote-broadcasted commitment transactions
	pub payment_key: SecretKey,
	/// Local secret key used in HTLC tx
	pub delayed_payment_base_key: SecretKey,
	/// Local htlc secret key used in commitment tx htlc outputs
	pub htlc_base_key: SecretKey,
	/// Commitment seed
	pub commitment_seed: [u8; 32],
	/// Local public keys and basepoints
	pub(crate) local_channel_pubkeys: ChannelPublicKeys,
	/// Remote public keys and base points
	pub(crate) remote_channel_pubkeys: Option<ChannelPublicKeys>,
	/// The total value of this channel
	channel_value_satoshis: u64,
	/// Key derivation parameters
	key_derivation_params: (u64, u64),
}

impl InMemoryChannelKeys {
	/// Create a new InMemoryChannelKeys
	pub fn new<C: Signing>(
		secp_ctx: &Secp256k1<C>,
		funding_key: SecretKey,
		revocation_base_key: SecretKey,
		payment_key: SecretKey,
		delayed_payment_base_key: SecretKey,
		htlc_base_key: SecretKey,
		commitment_seed: [u8; 32],
		channel_value_satoshis: u64,
		key_derivation_params: (u64, u64)) -> InMemoryChannelKeys {
		let local_channel_pubkeys =
			InMemoryChannelKeys::make_local_keys(secp_ctx, &funding_key, &revocation_base_key,
			                                     &payment_key, &delayed_payment_base_key,
			                                     &htlc_base_key);
		InMemoryChannelKeys {
			funding_key,
			revocation_base_key,
			payment_key,
			delayed_payment_base_key,
			htlc_base_key,
			commitment_seed,
			channel_value_satoshis,
			local_channel_pubkeys,
			remote_channel_pubkeys: None,
			key_derivation_params,
		}
	}

	fn make_local_keys<C: Signing>(secp_ctx: &Secp256k1<C>,
	                               funding_key: &SecretKey,
	                               revocation_base_key: &SecretKey,
	                               payment_key: &SecretKey,
	                               delayed_payment_base_key: &SecretKey,
	                               htlc_base_key: &SecretKey) -> ChannelPublicKeys {
		let from_secret = |s: &SecretKey| PublicKey::from_secret_key(secp_ctx, s);
		ChannelPublicKeys {
			funding_pubkey: from_secret(&funding_key),
			revocation_basepoint: from_secret(&revocation_base_key),
			payment_point: from_secret(&payment_key),
			delayed_payment_basepoint: from_secret(&delayed_payment_base_key),
			htlc_basepoint: from_secret(&htlc_base_key),
		}
	}

	fn remote_pubkeys<'a>(&'a self) -> &'a ChannelPublicKeys { self.remote_channel_pubkeys.as_ref().unwrap() }
}

impl ChannelKeys for InMemoryChannelKeys {
	fn commitment_seed(&self) -> &[u8; 32] { &self.commitment_seed }
	fn pubkeys(&self) -> &ChannelPublicKeys { &self.local_channel_pubkeys }
	fn key_derivation_params(&self) -> (u64, u64) { self.key_derivation_params }

	fn sign_remote_commitment<T: secp256k1::Signing + secp256k1::Verification>(&self, feerate_per_kw: u32, commitment_tx: &Transaction, keys: &TxCreationKeys, htlcs: &[&HTLCOutputInCommitment], to_self_delay: u16, secp_ctx: &Secp256k1<T>) -> Result<(Signature, Vec<Signature>), ()> {
		if commitment_tx.input.len() != 1 { return Err(()); }

		let funding_pubkey = PublicKey::from_secret_key(secp_ctx, &self.funding_key);
		let remote_channel_pubkeys = self.remote_channel_pubkeys.as_ref().expect("must set remote channel pubkeys before signing");
		let channel_funding_redeemscript = make_funding_redeemscript(&funding_pubkey, &remote_channel_pubkeys.funding_pubkey);

		let commitment_sighash = hash_to_message!(&bip143::SighashComponents::new(&commitment_tx).sighash_all(&commitment_tx.input[0], &channel_funding_redeemscript, self.channel_value_satoshis)[..]);
		let commitment_sig = secp_ctx.sign(&commitment_sighash, &self.funding_key);

		let commitment_txid = commitment_tx.txid();

		let mut htlc_sigs = Vec::with_capacity(htlcs.len());
		for ref htlc in htlcs {
			if let Some(_) = htlc.transaction_output_index {
				let htlc_tx = chan_utils::build_htlc_transaction(&commitment_txid, feerate_per_kw, to_self_delay, htlc, &keys.a_delayed_payment_key, &keys.revocation_key);
				let htlc_redeemscript = chan_utils::get_htlc_redeemscript(&htlc, &keys);
				let htlc_sighash = hash_to_message!(&bip143::SighashComponents::new(&htlc_tx).sighash_all(&htlc_tx.input[0], &htlc_redeemscript, htlc.amount_msat / 1000)[..]);
				let our_htlc_key = match chan_utils::derive_private_key(&secp_ctx, &keys.per_commitment_point, &self.htlc_base_key) {
					Ok(s) => s,
					Err(_) => return Err(()),
				};
				htlc_sigs.push(secp_ctx.sign(&htlc_sighash, &our_htlc_key));
			}
		}

		Ok((commitment_sig, htlc_sigs))
	}

	fn sign_local_commitment<T: secp256k1::Signing + secp256k1::Verification>(&self, local_commitment_tx: &LocalCommitmentTransaction, secp_ctx: &Secp256k1<T>) -> Result<Signature, ()> {
		let funding_pubkey = PublicKey::from_secret_key(secp_ctx, &self.funding_key);
		let remote_channel_pubkeys = self.remote_channel_pubkeys.as_ref().expect("must set remote channel pubkeys before signing");
		let channel_funding_redeemscript = make_funding_redeemscript(&funding_pubkey, &remote_channel_pubkeys.funding_pubkey);

		Ok(local_commitment_tx.get_local_sig(&self.funding_key, &channel_funding_redeemscript, self.channel_value_satoshis, secp_ctx))
	}

	#[cfg(test)]
	fn unsafe_sign_local_commitment<T: secp256k1::Signing + secp256k1::Verification>(&self, local_commitment_tx: &LocalCommitmentTransaction, secp_ctx: &Secp256k1<T>) -> Result<Signature, ()> {
		let funding_pubkey = PublicKey::from_secret_key(secp_ctx, &self.funding_key);
		let remote_channel_pubkeys = self.remote_channel_pubkeys.as_ref().expect("must set remote channel pubkeys before signing");
		let channel_funding_redeemscript = make_funding_redeemscript(&funding_pubkey, &remote_channel_pubkeys.funding_pubkey);

		Ok(local_commitment_tx.get_local_sig(&self.funding_key, &channel_funding_redeemscript, self.channel_value_satoshis, secp_ctx))
	}

	fn sign_local_commitment_htlc_transactions<T: secp256k1::Signing + secp256k1::Verification>(&self, local_commitment_tx: &LocalCommitmentTransaction, local_csv: u16, secp_ctx: &Secp256k1<T>) -> Result<Vec<Option<Signature>>, ()> {
		local_commitment_tx.get_htlc_sigs(&self.htlc_base_key, local_csv, secp_ctx)
	}

	fn sign_justice_transaction<T: secp256k1::Signing + secp256k1::Verification>(&self, justice_tx: &Transaction, input: usize, amount: u64, per_commitment_key: &SecretKey, htlc: &Option<HTLCOutputInCommitment>, on_remote_tx_csv: u16, secp_ctx: &Secp256k1<T>) -> Result<Signature, ()> {
		let revocation_key = match chan_utils::derive_private_revocation_key(&secp_ctx, &per_commitment_key, &self.revocation_base_key) {
			Ok(revocation_key) => revocation_key,
			Err(_) => return Err(())
		};
		let per_commitment_point = PublicKey::from_secret_key(secp_ctx, &per_commitment_key);
		let revocation_pubkey = match chan_utils::derive_public_revocation_key(&secp_ctx, &per_commitment_point, &self.pubkeys().revocation_basepoint) {
			Ok(revocation_pubkey) => revocation_pubkey,
			Err(_) => return Err(())
		};
		let witness_script = if let &Some(ref htlc) = htlc {
			let remote_htlcpubkey = match chan_utils::derive_public_key(&secp_ctx, &per_commitment_point, &self.remote_pubkeys().htlc_basepoint) {
				Ok(remote_htlcpubkey) => remote_htlcpubkey,
				Err(_) => return Err(())
			};
			let local_htlcpubkey = match chan_utils::derive_public_key(&secp_ctx, &per_commitment_point, &self.pubkeys().htlc_basepoint) {
				Ok(local_htlcpubkey) => local_htlcpubkey,
				Err(_) => return Err(())
			};
			chan_utils::get_htlc_redeemscript_with_explicit_keys(&htlc, &remote_htlcpubkey, &local_htlcpubkey, &revocation_pubkey)
		} else {
			let remote_delayedpubkey = match chan_utils::derive_public_key(&secp_ctx, &per_commitment_point, &self.remote_pubkeys().delayed_payment_basepoint) {
				Ok(remote_delayedpubkey) => remote_delayedpubkey,
				Err(_) => return Err(())
			};
			chan_utils::get_revokeable_redeemscript(&revocation_pubkey, on_remote_tx_csv, &remote_delayedpubkey)
		};
		let sighash_parts = bip143::SighashComponents::new(&justice_tx);
		let sighash = hash_to_message!(&sighash_parts.sighash_all(&justice_tx.input[input], &witness_script, amount)[..]);
		return Ok(secp_ctx.sign(&sighash, &revocation_key))
	}

	fn sign_remote_htlc_transaction<T: secp256k1::Signing + secp256k1::Verification>(&self, htlc_tx: &Transaction, input: usize, amount: u64, per_commitment_point: &PublicKey, htlc: &HTLCOutputInCommitment, secp_ctx: &Secp256k1<T>) -> Result<Signature, ()> {
		if let Ok(htlc_key) = chan_utils::derive_private_key(&secp_ctx, &per_commitment_point, &self.htlc_base_key) {
			let witness_script = if let Ok(revocation_pubkey) = chan_utils::derive_public_revocation_key(&secp_ctx, &per_commitment_point, &self.pubkeys().revocation_basepoint) {
				if let Ok(remote_htlcpubkey) = chan_utils::derive_public_key(&secp_ctx, &per_commitment_point, &self.remote_pubkeys().htlc_basepoint) {
					if let Ok(local_htlcpubkey) = chan_utils::derive_public_key(&secp_ctx, &per_commitment_point, &self.pubkeys().htlc_basepoint) {
						chan_utils::get_htlc_redeemscript_with_explicit_keys(&htlc, &remote_htlcpubkey, &local_htlcpubkey, &revocation_pubkey)
					} else { return Err(()) }
				} else { return Err(()) }
			} else { return Err(()) };
			let sighash_parts = bip143::SighashComponents::new(&htlc_tx);
			let sighash = hash_to_message!(&sighash_parts.sighash_all(&htlc_tx.input[input], &witness_script, amount)[..]);
			return Ok(secp_ctx.sign(&sighash, &htlc_key))
		}
		Err(())
	}

	fn sign_closing_transaction<T: secp256k1::Signing>(&self, closing_tx: &Transaction, secp_ctx: &Secp256k1<T>) -> Result<Signature, ()> {
		if closing_tx.input.len() != 1 { return Err(()); }
		if closing_tx.input[0].witness.len() != 0 { return Err(()); }
		if closing_tx.output.len() > 2 { return Err(()); }

		let remote_channel_pubkeys = self.remote_channel_pubkeys.as_ref().expect("must set remote channel pubkeys before signing");
		let funding_pubkey = PublicKey::from_secret_key(secp_ctx, &self.funding_key);
		let channel_funding_redeemscript = make_funding_redeemscript(&funding_pubkey, &remote_channel_pubkeys.funding_pubkey);

		let sighash = hash_to_message!(&bip143::SighashComponents::new(closing_tx)
			.sighash_all(&closing_tx.input[0], &channel_funding_redeemscript, self.channel_value_satoshis)[..]);
		Ok(secp_ctx.sign(&sighash, &self.funding_key))
	}

	fn sign_channel_announcement<T: secp256k1::Signing>(&self, msg: &msgs::UnsignedChannelAnnouncement, secp_ctx: &Secp256k1<T>) -> Result<Signature, ()> {
		let msghash = hash_to_message!(&Sha256dHash::hash(&msg.encode()[..])[..]);
		Ok(secp_ctx.sign(&msghash, &self.funding_key))
	}

	fn set_remote_channel_pubkeys(&mut self, channel_pubkeys: &ChannelPublicKeys) {
		assert!(self.remote_channel_pubkeys.is_none(), "Already set remote channel pubkeys");
		self.remote_channel_pubkeys = Some(channel_pubkeys.clone());
	}
}

impl Writeable for InMemoryChannelKeys {
	fn write<W: Writer>(&self, writer: &mut W) -> Result<(), Error> {
		self.funding_key.write(writer)?;
		self.revocation_base_key.write(writer)?;
		self.payment_key.write(writer)?;
		self.delayed_payment_base_key.write(writer)?;
		self.htlc_base_key.write(writer)?;
		self.commitment_seed.write(writer)?;
		self.remote_channel_pubkeys.write(writer)?;
		self.channel_value_satoshis.write(writer)?;
		self.key_derivation_params.0.write(writer)?;
		self.key_derivation_params.1.write(writer)?;

		Ok(())
	}
}

impl Readable for InMemoryChannelKeys {
	fn read<R: ::std::io::Read>(reader: &mut R) -> Result<Self, DecodeError> {
		let funding_key = Readable::read(reader)?;
		let revocation_base_key = Readable::read(reader)?;
		let payment_key = Readable::read(reader)?;
		let delayed_payment_base_key = Readable::read(reader)?;
		let htlc_base_key = Readable::read(reader)?;
		let commitment_seed = Readable::read(reader)?;
		let remote_channel_pubkeys = Readable::read(reader)?;
		let channel_value_satoshis = Readable::read(reader)?;
		let secp_ctx = Secp256k1::signing_only();
		let local_channel_pubkeys =
			InMemoryChannelKeys::make_local_keys(&secp_ctx, &funding_key, &revocation_base_key,
			                                     &payment_key, &delayed_payment_base_key,
			                                     &htlc_base_key);
		let params_1 = Readable::read(reader)?;
		let params_2 = Readable::read(reader)?;

		Ok(InMemoryChannelKeys {
			funding_key,
			revocation_base_key,
			payment_key,
			delayed_payment_base_key,
			htlc_base_key,
			commitment_seed,
			channel_value_satoshis,
			local_channel_pubkeys,
			remote_channel_pubkeys,
			key_derivation_params: (params_1, params_2),
		})
	}
}

/// Simple KeysInterface implementor that takes a 32-byte seed for use as a BIP 32 extended key
/// and derives keys from that.
///
/// Your node_id is seed/0'
/// ChannelMonitor closes may use seed/1'
/// Cooperative closes may use seed/2'
/// The two close keys may be needed to claim on-chain funds!
pub struct KeysManager {
	secp_ctx: Secp256k1<secp256k1::SignOnly>,
	node_secret: SecretKey,
	destination_script: Script,
	shutdown_pubkey: PublicKey,
	channel_master_key: ExtendedPrivKey,
	channel_child_index: AtomicUsize,
	session_master_key: ExtendedPrivKey,
	session_child_index: AtomicUsize,
	channel_id_master_key: ExtendedPrivKey,
	channel_id_child_index: AtomicUsize,

	seed: [u8; 32],
	starting_time_secs: u64,
	starting_time_nanos: u32,
}

impl KeysManager {
	/// Constructs a KeysManager from a 32-byte seed. If the seed is in some way biased (eg your
	/// RNG is busted) this may panic (but more importantly, you will possibly lose funds).
	/// starting_time isn't strictly required to actually be a time, but it must absolutely,
	/// without a doubt, be unique to this instance. ie if you start multiple times with the same
	/// seed, starting_time must be unique to each run. Thus, the easiest way to achieve this is to
	/// simply use the current time (with very high precision).
	///
	/// The seed MUST be backed up safely prior to use so that the keys can be re-created, however,
	/// obviously, starting_time should be unique every time you reload the library - it is only
	/// used to generate new ephemeral key data (which will be stored by the individual channel if
	/// necessary).
	///
	/// Note that the seed is required to recover certain on-chain funds independent of
	/// ChannelMonitor data, though a current copy of ChannelMonitor data is also required for any
	/// channel, and some on-chain during-closing funds.
	///
	/// Note that until the 0.1 release there is no guarantee of backward compatibility between
	/// versions. Once the library is more fully supported, the docs will be updated to include a
	/// detailed description of the guarantee.
	pub fn new(seed: &[u8; 32], network: Network, starting_time_secs: u64, starting_time_nanos: u32) -> Self {
		let secp_ctx = Secp256k1::signing_only();
		match ExtendedPrivKey::new_master(network.clone(), seed) {
			Ok(master_key) => {
				let node_secret = master_key.ckd_priv(&secp_ctx, ChildNumber::from_hardened_idx(0).unwrap()).expect("Your RNG is busted").private_key.key;
				let destination_script = match master_key.ckd_priv(&secp_ctx, ChildNumber::from_hardened_idx(1).unwrap()) {
					Ok(destination_key) => {
						let wpubkey_hash = WPubkeyHash::hash(&ExtendedPubKey::from_private(&secp_ctx, &destination_key).public_key.to_bytes());
						Builder::new().push_opcode(opcodes::all::OP_PUSHBYTES_0)
						              .push_slice(&wpubkey_hash.into_inner())
						              .into_script()
					},
					Err(_) => panic!("Your RNG is busted"),
				};
				let shutdown_pubkey = match master_key.ckd_priv(&secp_ctx, ChildNumber::from_hardened_idx(2).unwrap()) {
					Ok(shutdown_key) => ExtendedPubKey::from_private(&secp_ctx, &shutdown_key).public_key.key,
					Err(_) => panic!("Your RNG is busted"),
				};
				let channel_master_key = master_key.ckd_priv(&secp_ctx, ChildNumber::from_hardened_idx(3).unwrap()).expect("Your RNG is busted");
				let session_master_key = master_key.ckd_priv(&secp_ctx, ChildNumber::from_hardened_idx(4).unwrap()).expect("Your RNG is busted");
				let channel_id_master_key = master_key.ckd_priv(&secp_ctx, ChildNumber::from_hardened_idx(5).unwrap()).expect("Your RNG is busted");

				KeysManager {
					secp_ctx,
					node_secret,
					destination_script,
					shutdown_pubkey,
					channel_master_key,
					channel_child_index: AtomicUsize::new(0),
					session_master_key,
					session_child_index: AtomicUsize::new(0),
					channel_id_master_key,
					channel_id_child_index: AtomicUsize::new(0),

					seed: *seed,
					starting_time_secs,
					starting_time_nanos,
				}
			},
			Err(_) => panic!("Your rng is busted"),
		}
	}
	fn derive_unique_start(&self) -> Sha256State {
		let mut unique_start = Sha256::engine();
		unique_start.input(&byte_utils::be64_to_array(self.starting_time_secs));
		unique_start.input(&byte_utils::be32_to_array(self.starting_time_nanos));
		unique_start.input(&self.seed);
		unique_start
	}
	/// Derive an old set of ChannelKeys for per-channel secrets based on a key derivation
	/// parameters.
	/// Key derivation parameters are accessible through a per-channel secrets
	/// ChannelKeys::key_derivation_params and is provided inside DynamicOuputP2WSH in case of
	/// onchain output detection for which a corresponding delayed_payment_key must be derived.
	pub fn derive_channel_keys(&self, channel_value_satoshis: u64, params_1: u64, params_2: u64) -> InMemoryChannelKeys {
		let chan_id = ((params_1 & 0xFFFF_FFFF_0000_0000) >> 32) as u32;
		let mut unique_start = Sha256::engine();
		unique_start.input(&byte_utils::be64_to_array(params_2));
		unique_start.input(&byte_utils::be32_to_array(params_1 as u32));
		unique_start.input(&self.seed);

		// We only seriously intend to rely on the channel_master_key for true secure
		// entropy, everything else just ensures uniqueness. We rely on the unique_start (ie
		// starting_time provided in the constructor) to be unique.
		let child_privkey = self.channel_master_key.ckd_priv(&self.secp_ctx, ChildNumber::from_hardened_idx(chan_id).expect("key space exhausted")).expect("Your RNG is busted");
		unique_start.input(&child_privkey.private_key.key[..]);

		let seed = Sha256::from_engine(unique_start).into_inner();

		let commitment_seed = {
			let mut sha = Sha256::engine();
			sha.input(&seed);
			sha.input(&b"commitment seed"[..]);
			Sha256::from_engine(sha).into_inner()
		};
		macro_rules! key_step {
			($info: expr, $prev_key: expr) => {{
				let mut sha = Sha256::engine();
				sha.input(&seed);
				sha.input(&$prev_key[..]);
				sha.input(&$info[..]);
				SecretKey::from_slice(&Sha256::from_engine(sha).into_inner()).expect("SHA-256 is busted")
			}}
		}
		let funding_key = key_step!(b"funding key", commitment_seed);
		let revocation_base_key = key_step!(b"revocation base key", funding_key);
		let payment_key = key_step!(b"payment key", revocation_base_key);
		let delayed_payment_base_key = key_step!(b"delayed payment base key", payment_key);
		let htlc_base_key = key_step!(b"HTLC base key", delayed_payment_base_key);

		InMemoryChannelKeys::new(
			&self.secp_ctx,
			funding_key,
			revocation_base_key,
			payment_key,
			delayed_payment_base_key,
			htlc_base_key,
			commitment_seed,
			channel_value_satoshis,
			(params_1, params_2),
		)
	}
}

impl KeysInterface for KeysManager {
	type ChanKeySigner = InMemoryChannelKeys;

	fn get_node_secret(&self) -> SecretKey {
		self.node_secret.clone()
	}

	fn get_destination_script(&self) -> Script {
		self.destination_script.clone()
	}

	fn get_shutdown_pubkey(&self) -> PublicKey {
		self.shutdown_pubkey.clone()
	}

	fn get_channel_keys(&self, _inbound: bool, channel_value_satoshis: u64) -> InMemoryChannelKeys {
		let child_ix = self.channel_child_index.fetch_add(1, Ordering::AcqRel);
		let ix_and_nanos: u64 = (child_ix as u64) << 32 | (self.starting_time_nanos as u64);
		self.derive_channel_keys(channel_value_satoshis, ix_and_nanos, self.starting_time_secs)
	}

	fn get_onion_rand(&self) -> (SecretKey, [u8; 32]) {
		let mut sha = self.derive_unique_start();

		let child_ix = self.session_child_index.fetch_add(1, Ordering::AcqRel);
		let child_privkey = self.session_master_key.ckd_priv(&self.secp_ctx, ChildNumber::from_hardened_idx(child_ix as u32).expect("key space exhausted")).expect("Your RNG is busted");
		sha.input(&child_privkey.private_key.key[..]);

		let mut rng_seed = sha.clone();
		// Not exactly the most ideal construction, but the second value will get fed into
		// ChaCha so it is another step harder to break.
		rng_seed.input(b"RNG Seed Salt");
		sha.input(b"Session Key Salt");
		(SecretKey::from_slice(&Sha256::from_engine(sha).into_inner()).expect("Your RNG is busted"),
		Sha256::from_engine(rng_seed).into_inner())
	}

	fn get_channel_id(&self) -> [u8; 32] {
		let mut sha = self.derive_unique_start();

		let child_ix = self.channel_id_child_index.fetch_add(1, Ordering::AcqRel);
		let child_privkey = self.channel_id_master_key.ckd_priv(&self.secp_ctx, ChildNumber::from_hardened_idx(child_ix as u32).expect("key space exhausted")).expect("Your RNG is busted");
		sha.input(&child_privkey.private_key.key[..]);

		Sha256::from_engine(sha).into_inner()
	}
}
