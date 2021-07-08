///! Traits and default implementation for paying transaction fees in assets.
use crate::Config;
use super::*;
use codec::FullCodec;
use frame_support::{
	traits::{Currency, ExistenceRequirement, Get, Imbalance, OnUnbalanced, fungibles::{Balanced, Inspect, CreditOf}, WithdrawReasons},
	unsigned::TransactionValidityError,
};
use pallet_assets::BalanceConversion;
use sp_runtime::{
	traits::{AtLeast32BitUnsigned, DispatchInfoOf, MaybeSerializeDeserialize, PostDispatchInfoOf, Saturating, Zero},
	transaction_validity::InvalidTransaction,
};
use sp_std::{fmt::Debug, marker::PhantomData};

/// Handle withdrawing, refunding and depositing of transaction fees.
pub trait OnChargeAssetTransaction<T: Config> {
	/// The underlying integer type in which fees are calculated.
	type Balance: AtLeast32BitUnsigned + FullCodec + Copy + MaybeSerializeDeserialize + Debug + Default;
	/// The type used to identify the assets used for transaction payment.
	type AssetId: FullCodec + Copy + MaybeSerializeDeserialize + Debug + Default + Eq;
	/// The type used to store the intermediate values between pre- and post-dispatch.
	type LiquidityInfo;

	/// Before the transaction is executed the payment of the transaction fees
	/// needs to be secured.
	///
	/// Note: The `fee` already includes the `tip`.
	fn withdraw_fee(
		who: &T::AccountId,
		call: &T::Call,
		dispatch_info: &DispatchInfoOf<T::Call>,
		asset_id: Self::AssetId,
		fee: Self::Balance,
		tip: Self::Balance,
	) -> Result<Self::LiquidityInfo, TransactionValidityError>;

	/// After the transaction was executed the actual fee can be calculated.
	/// This function should refund any overpaid fees and optionally deposit
	/// the corrected amount.
	///
	/// Note: The `fee` already includes the `tip`.
	fn correct_and_deposit_fee(
		who: &T::AccountId,
		dispatch_info: &DispatchInfoOf<T::Call>,
		post_info: &PostDispatchInfoOf<T::Call>,
		corrected_fee: Self::Balance,
		tip: Self::Balance,
		already_withdrawn: Self::LiquidityInfo,
	) -> Result<(), TransactionValidityError>;
}

/// Allows specifying what to do with the withdrawn asset fees.
pub trait HandleCredit<AccountId, B: Balanced<AccountId>> {
	/// Implement to determine what to do with the withdrawn asset fees.
	/// Default for `CreditOf` from the assets pallet is to burn and
	/// decrease total issuance.
	fn handle_credit(credit: CreditOf<AccountId, B>);
}

/// Implements the asset transaction for a balance to asset converter (implementing `BalanceConversion`)
/// and a credit handler (implementing `HandleCredit`).
///
/// The credit handler is given the complete fee in terms of
/// the asset used for the transaction.
pub struct FungiblesAdapter<CON, HC>(PhantomData<(CON, HC)>);

/// Default implementation for a runtime instantiating this pallet, a balance to asset converter
/// and a credit handler.
impl<T, CON, HC> OnChargeAssetTransaction<T> for FungiblesAdapter<CON, HC>
where
	T: Config,
	CON: BalanceConversion<BalanceOf<T>, AssetIdOf<T>, AssetBalanceOf<T>>,
	HC: HandleCredit<T::AccountId, T::Fungibles>,
	AssetIdOf<T>: FullCodec + Copy + MaybeSerializeDeserialize + Debug + Default + Eq,
{
	type LiquidityInfo = CreditOf<T::AccountId, T::Fungibles>;
	type Balance = BalanceOf<T>;
	type AssetId = AssetIdOf<T>;

	/// Withdraw the predicted fee from the transaction origin.
	///
	/// Note: The `fee` already includes the `tip`.
	fn withdraw_fee(
		who: &T::AccountId,
		_call: &T::Call,
		_info: &DispatchInfoOf<T::Call>,
		asset_id: Self::AssetId,
		fee: Self::Balance,
		_tip: Self::Balance,
	) -> Result<Self::LiquidityInfo, TransactionValidityError> {
		let converted_fee = CON::to_asset_balance(fee, asset_id)
			.map_err(|_| TransactionValidityError::from(InvalidTransaction::Payment))?;
		let can_withdraw = <T::Fungibles as Inspect<T::AccountId>>::can_withdraw(asset_id.into(), who, converted_fee);
		if !matches!(can_withdraw, WithdrawConsequence::Success) {
			return Err(InvalidTransaction::Payment.into());
		}
		<T::Fungibles as Balanced<T::AccountId>>::withdraw(asset_id.into(), who, converted_fee)
			.map_err(|_| TransactionValidityError::from(InvalidTransaction::Payment))
	}

	/// Hand the fee and the tip over to the `[HandleCredit]` implementation.
	/// Since the predicted fee might have been too high, parts of the fee may
	/// be refunded.
	///
	/// Note: The `corrected_fee` already includes the `tip`.
	fn correct_and_deposit_fee(
		who: &T::AccountId,
		_dispatch_info: &DispatchInfoOf<T::Call>,
		_post_info: &PostDispatchInfoOf<T::Call>,
		corrected_fee: Self::Balance,
		_tip: Self::Balance,
		paid: Self::LiquidityInfo,
	) -> Result<(), TransactionValidityError> {
		// Convert the corrected fee into the asset used for payment.
		let converted_fee = CON::to_asset_balance(corrected_fee, paid.asset().into())
			.map_err(|_| -> TransactionValidityError { InvalidTransaction::Payment.into() })?;
		// Calculate how much refund we should return.
		let (final_fee, refund) = paid.split(converted_fee);
		// Refund to the the account that paid the fees. If this fails, the
		// account might have dropped below the existential balance. In
		// that case we don't refund anything.
		let _res = <T::Fungibles as Balanced<T::AccountId>>::resolve(who, refund);
		// Handle the final fee, e.g. by transferring to the block author or burning.
		HC::handle_credit(final_fee);
		Ok(())
	}
}
