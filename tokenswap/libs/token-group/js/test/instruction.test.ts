import { expect } from 'chai';
import type { StructToDecoderTuple } from '@solana/codecs-data-structures';
import { getBytesDecoder, getStructDecoder } from '@solana/codecs-data-structures';
import { splDiscriminate } from '@solana/spl-type-length-value';
import { getU32Decoder } from '@solana/codecs-numbers';
import { PublicKey, type TransactionInstruction } from '@solana/web3.js';

import {
    createInitializeGroupInstruction,
    createInitializeMemberInstruction,
    createUpdateGroupMaxSizeInstruction,
    createUpdateGroupAuthorityInstruction,
} from '../src';

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

describe('Token Group Instructions', () => {
    const programId = new PublicKey('22222222222222222222222222222222222222222222');
    const group = new PublicKey('33333333333333333333333333333333333333333333');
    const updateAuthority = new PublicKey('44444444444444444444444444444444444444444444');
    const mint = new PublicKey('55555555555555555555555555555555555555555555');
    const mintAuthority = new PublicKey('66666666666666666666666666666666666666666666');
    const maxSize = 100;

    it('Can create InitializeGroup Instruction', () => {
        checkPackUnpack(
            createInitializeGroupInstruction({
                programId,
                group,
                mint,
                mintAuthority,
                updateAuthority,
                maxSize,
            }),
            splDiscriminate('spl_token_group_interface:initialize_token_group'),
            [
                ['updateAuthority', getBytesDecoder({ size: 32 })],
                ['maxSize', getU32Decoder()],
            ],
            { updateAuthority: Uint8Array.from(updateAuthority.toBuffer()), maxSize }
        );
    });

    it('Can create UpdateGroupMaxSize Instruction', () => {
        checkPackUnpack(
            createUpdateGroupMaxSizeInstruction({
                programId,
                group,
                updateAuthority,
                maxSize,
            }),
            splDiscriminate('spl_token_group_interface:update_group_max_size'),
            [['maxSize', getU32Decoder()]],
            { maxSize }
        );
    });

    it('Can create UpdateGroupAuthority Instruction', () => {
        checkPackUnpack(
            createUpdateGroupAuthorityInstruction({
                programId,
                group,
                currentAuthority: updateAuthority,
                newAuthority: PublicKey.default,
            }),
            splDiscriminate('spl_token_group_interface:update_authority'),
            [['newAuthority', getBytesDecoder({ size: 32 })]],
            { newAuthority: Uint8Array.from(PublicKey.default.toBuffer()) }
        );
    });

    it('Can create InitializeMember Instruction', () => {
        const member = new PublicKey('22222222222222222222222222222222222222222222');
        const memberMint = new PublicKey('33333333333333333333333333333333333333333333');
        const memberMintAuthority = new PublicKey('44444444444444444444444444444444444444444444');
        const group = new PublicKey('55555555555555555555555555555555555555555555');
        const groupUpdateAuthority = new PublicKey('66666666666666666666666666666666666666666666');

        checkPackUnpack(
            createInitializeMemberInstruction({
                programId,
                member,
                memberMint,
                memberMintAuthority,
                group,
                groupUpdateAuthority,
            }),
            splDiscriminate('spl_token_group_interface:initialize_member'),
            [],
            {}
        );
    });
});
