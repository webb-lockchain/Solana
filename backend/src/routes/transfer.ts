import express, {Request,Response} from 'express';
import dotenv from 'dotenv';
import * as web3 from '@solana/web3.js';
import { BN, Program } from '@project-serum/anchor';
import { TOKEN_PROGRAM_ID, getAssociatedTokenAddressSync } from '@solana/spl-token';
import { PublicKey, SystemProgram } from '@solana/web3.js';

dotenv.config();
const router=express.Router();

// const apiUrl = process.env.url;
// const publicKey = process.env.publickey;


// // Connect to the Solana cluster
// const connection = new web3.Connection(`${apiUrl}`);
// // Define the public key of the smart contract
// const programId = new web3.PublicKey(`${publicKey}`);


router.get('/api/transfer',[],(req:Request,res:Response)=>{
    // console.log(`API URL: ${apiUrl}`);
    // console.log(`Pubkic Key: ${publicKey}`);
    // console.log(programId);
    // return res.send('transfer')
})

export {router as transferRouter}