import { expect } from 'chai';

import {
    createEmitInstruction,
    createInitializeInstruction,
    createRemoveKeyInstruction,
    createUpdateAuthorityInstruction,
    createUpdateFieldInstruction,
    getFieldCodec,
    getFieldConfig,
} from '../src';
import type { StructToDecoderTuple } from '@solana/codecs-data-structures';
import { getBooleanDecoder, getBytesDecoder, getDataEnumCodec, getStructDecoder } from '@solana/codecs-data-structures';
import { getStringDecoder } from '@solana/codecs-strings';
import { splDiscriminate } from '@solana/spl-type-length-value';
import { getU64Decoder } from '@solana/codecs-numbers';
import type { Option } from '@solana/options';
import { getOptionDecoder, some } from '@solana/options';
import { PublicKey, type TransactionInstruction } from '@solana/web3.js';

function checkPackUnpack<T extends object>(
    instruction: TransactionInstruction,
    discriminator: Uint8Array,
    layout: StructToDecoderTuple<T>,
    values: T
) {
    expect(instruction.data.subarray(0, 8)).to.deep.equal(discriminator);
    const decoder = getStructDecoder(layout);
    const unpacked = decoder.decode(instruction.data.subarray(8));
    expect(unpacked).to.deep.equal(values);
}

describe('Token Metadata Instructions', () => {
    const programId = new PublicKey('22222222222222222222222222222222222222222222');
    const metadata = new PublicKey('33333333333333333333333333333333333333333333');
    const updateAuthority = new PublicKey('44444444444444444444444444444444444444444444');
    const mint = new PublicKey('55555555555555555555555555555555555555555555');
    const mintAuthority = new PublicKey('66666666666666666666666666666666666666666666');

    it('Can create Initialize Instruction', () => {
        const name = 'My test token';
        const symbol = 'TEST';
        const uri = 'http://test.test';
        checkPackUnpack(
            createInitializeInstruction({
                programId,
                metadata,
                updateAuthority,
                mint,
                mintAuthority,
                name,
                symbol,
                uri,
            }),
            splDiscriminate('spl_token_metadata_interface:initialize_account'),
            [
                ['name', getStringDecoder()],
                ['symbol', getStringDecoder()],
                ['uri', getStringDecoder()],
            ],
            { name, symbol, uri }
        );
    });

    it('Can create Update Field Instruction', () => {
        const field = 'MyTestField';
        const value = 'http://test.uri';
        checkPackUnpack(
            createUpdateFieldInstruction({
                programId,
                metadata,
                updateAuthority,
                field,
                value,
            }),
            splDiscriminate('spl_token_metadata_interface:updating_field'),
            [
                ['key', getDataEnumCodec(getFieldCodec())],
                ['value', getStringDecoder()],
            ],
            { key: getFieldConfig(field), value }
        );
    });

    it('Can create Update Field Instruction with Field Enum', () => {
        const field = 'Name';
        const value = 'http://test.uri';
        checkPackUnpack(
            createUpdateFieldInstruction({
                programId,
                metadata,
                updateAuthority,
                field,
                value,
            }),
            splDiscriminate('spl_token_metadata_interface:updating_field'),
            [
                ['key', getDataEnumCodec(getFieldCodec())],
                ['value', getStringDecoder()],
            ],
            { key: getFieldConfig(field), value }
        );
    });

    it('Can create Remove Key Instruction', () => {
        checkPackUnpack(
            createRemoveKeyInstruction({
                programId,
                metadata,
                updateAuthority: updateAuthority,
                key: 'MyTestField',
                idempotent: true,
            }),
            splDiscriminate('spl_token_metadata_interface:remove_key_ix'),
            [
                ['idempotent', getBooleanDecoder()],
                ['key', getStringDecoder()],
            ],
            { idempotent: true, key: 'MyTestField' }
        );
    });

    it('Can create Update Authority Instruction', () => {
        const newAuthority = PublicKey.default;
        checkPackUnpack(
            createUpdateAuthorityInstruction({
                programId,
                metadata,
                oldAuthority: updateAuthority,
                newAuthority,
            }),
            splDiscriminate('spl_token_metadata_interface:update_the_authority'),
            [['newAuthority', getBytesDecoder({ size: 32 })]],
            { newAuthority: Uint8Array.from(newAuthority.toBuffer()) }
        );
    });

    it('Can create Emit Instruction', () => {
        const start: Option<bigint> = some(0n);
        const end: Option<bigint> = some(10n);
        checkPackUnpack(
            createEmitInstruction({
                programId,
                metadata,
                start: 0n,
                end: 10n,
            }),
            splDiscriminate('spl_token_metadata_interface:emitter'),
            [
                ['start', getOptionDecoder(getU64Decoder())],
                ['end', getOptionDecoder(getU64Decoder())],
            ],
            { start, end }
        );
    });
});
