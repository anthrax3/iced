/*
Copyright (C) 2018-2019 de4dot@gmail.com

Permission is hereby granted, free of charge, to any person obtaining
a copy of this software and associated documentation files (the
"Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to
the following conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
*/

use super::super::*;
use super::*;
#[cfg(not(feature = "std"))]
use alloc::string::String;
use core::cell::RefCell;
use core::{cmp, i32, i8, u32};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
enum InstrKind {
	Unchanged,
	Short,
	Near,
	Long,
	Uninitialized,
}

pub(super) struct SimpleBranchInstr {
	orig_ip: u64,
	ip: u64,
	block: Rc<RefCell<Block>>,
	size: u32,
	bitness: u32,
	instruction: Instruction,
	target_instr: TargetInstr,
	pointer_data: Option<Rc<RefCell<BlockData>>>,
	instr_kind: InstrKind,
	short_instruction_size: u32,
	near_instruction_size: u32,
	long_instruction_size: u32,
	native_instruction_size: u32,
	native_code: Code,
}

impl SimpleBranchInstr {
	pub fn new(block_encoder: &mut BlockEncoder, block: Rc<RefCell<Block>>, instruction: &Instruction) -> Self {
		let mut instr_kind = InstrKind::Uninitialized;
		let mut instr_copy;
		let native_code;
		let size;
		let short_instruction_size;
		let near_instruction_size;
		let long_instruction_size;
		let native_instruction_size;
		if !block_encoder.fix_branches() {
			instr_kind = InstrKind::Unchanged;
			instr_copy = *instruction;
			instr_copy.set_near_branch64(0);
			size = block_encoder.get_instruction_size(&instr_copy, 0);
			native_code = Code::INVALID;
			short_instruction_size = 0;
			near_instruction_size = 0;
			long_instruction_size = 0;
			native_instruction_size = 0;
		} else {
			instr_copy = *instruction;
			instr_copy.set_near_branch64(0);
			short_instruction_size = block_encoder.get_instruction_size(&instr_copy, 0);

			native_code = Self::as_native_branch_code(instruction.code(), block_encoder.bitness());
			native_instruction_size = if native_code == instruction.code() {
				short_instruction_size
			} else {
				instr_copy = *instruction;
				instr_copy.set_code(native_code);
				instr_copy.set_near_branch64(0);
				block_encoder.get_instruction_size(&instr_copy, 0)
			};

			near_instruction_size = match block_encoder.bitness() {
				16 => native_instruction_size + 2 + 3,
				32 | 64 => native_instruction_size + 2 + 5,
				_ => unreachable!(),
			};

			size = if block_encoder.bitness() == 64 {
				long_instruction_size = native_instruction_size + 2 + InstrUtils::CALL_OR_JMP_POINTER_DATA_INSTRUCTION_SIZE64;
				cmp::max(cmp::max(short_instruction_size, near_instruction_size), long_instruction_size)
			} else {
				long_instruction_size = 0;
				cmp::max(short_instruction_size, near_instruction_size)
			};
		}
		Self {
			orig_ip: instruction.ip(),
			ip: 0,
			block,
			size,
			bitness: block_encoder.bitness(),
			instruction: *instruction,
			target_instr: TargetInstr::default(),
			pointer_data: None,
			instr_kind,
			short_instruction_size,
			near_instruction_size,
			long_instruction_size,
			native_instruction_size,
			native_code,
		}
	}

	fn try_optimize(&mut self) -> bool {
		if self.instr_kind == InstrKind::Unchanged || self.instr_kind == InstrKind::Short {
			return false;
		}

		let mut target_address = self.target_instr.address(self);
		let mut next_rip = self.ip().wrapping_add(self.short_instruction_size as u64);
		let mut diff = target_address.wrapping_sub(next_rip) as i64;
		if i8::MIN as i64 <= diff && diff <= i8::MAX as i64 {
			if let Some(ref pointer_data) = self.pointer_data {
				pointer_data.borrow_mut().is_valid = false;
			}
			self.instr_kind = InstrKind::Short;
			self.size = self.short_instruction_size;
			return true;
		}

		// If it's in the same block, we assume the target is at most 2GB away.
		let mut use_near = self.bitness != 64 || self.target_instr.is_in_block(self.block.clone());
		if !use_near {
			target_address = self.target_instr.address(self);
			next_rip = self.ip.wrapping_add(self.near_instruction_size as u64);
			diff = target_address.wrapping_sub(next_rip) as i64;
			use_near = i32::MIN as i64 <= diff && diff <= i32::MAX as i64;
		}
		if use_near {
			if let Some(ref pointer_data) = self.pointer_data {
				pointer_data.borrow_mut().is_valid = false;
			}
			self.instr_kind = InstrKind::Near;
			self.size = self.near_instruction_size;
			return true;
		}

		if self.pointer_data.is_none() {
			// Temp needed if rustc < 1.36.0 (2015 edition)
			let tmp = self.block.clone();
			self.pointer_data = Some(tmp.borrow_mut().alloc_pointer_location());
		}
		self.instr_kind = InstrKind::Long;
		false
	}

	#[cfg_attr(feature = "cargo-clippy", allow(clippy::wrong_self_convention))]
	fn as_native_branch_code(code: Code, bitness: u32) -> Code {
		let (c16, c32, c64) = match code {
			Code::Loopne_rel8_16_CX | Code::Loopne_rel8_32_CX => (Code::Loopne_rel8_16_CX, Code::Loopne_rel8_32_CX, Code::INVALID),
			Code::Loopne_rel8_16_ECX | Code::Loopne_rel8_32_ECX | Code::Loopne_rel8_64_ECX => {
				(Code::Loopne_rel8_16_ECX, Code::Loopne_rel8_32_ECX, Code::Loopne_rel8_64_ECX)
			}
			Code::Loopne_rel8_16_RCX | Code::Loopne_rel8_64_RCX => (Code::Loopne_rel8_16_RCX, Code::INVALID, Code::Loopne_rel8_64_RCX),
			Code::Loope_rel8_16_CX | Code::Loope_rel8_32_CX => (Code::Loope_rel8_16_CX, Code::Loope_rel8_32_CX, Code::INVALID),
			Code::Loope_rel8_16_ECX | Code::Loope_rel8_32_ECX | Code::Loope_rel8_64_ECX => {
				(Code::Loope_rel8_16_ECX, Code::Loope_rel8_32_ECX, Code::Loope_rel8_64_ECX)
			}
			Code::Loope_rel8_16_RCX | Code::Loope_rel8_64_RCX => (Code::Loope_rel8_16_RCX, Code::INVALID, Code::Loope_rel8_64_RCX),
			Code::Loop_rel8_16_CX | Code::Loop_rel8_32_CX => (Code::Loop_rel8_16_CX, Code::Loop_rel8_32_CX, Code::INVALID),
			Code::Loop_rel8_16_ECX | Code::Loop_rel8_32_ECX | Code::Loop_rel8_64_ECX => {
				(Code::Loop_rel8_16_ECX, Code::Loop_rel8_32_ECX, Code::Loop_rel8_64_ECX)
			}
			Code::Loop_rel8_16_RCX | Code::Loop_rel8_64_RCX => (Code::Loop_rel8_16_RCX, Code::INVALID, Code::Loop_rel8_64_RCX),
			Code::Jcxz_rel8_16 | Code::Jcxz_rel8_32 => (Code::Jcxz_rel8_16, Code::Jcxz_rel8_32, Code::INVALID),
			Code::Jecxz_rel8_16 | Code::Jecxz_rel8_32 | Code::Jecxz_rel8_64 => (Code::Jecxz_rel8_16, Code::Jecxz_rel8_32, Code::Jecxz_rel8_64),
			Code::Jrcxz_rel8_16 | Code::Jrcxz_rel8_64 => (Code::Jrcxz_rel8_16, Code::INVALID, Code::Jrcxz_rel8_64),
			_ => unreachable!(),
		};

		match bitness {
			16 => c16,
			32 => c32,
			64 => c64,
			_ => unreachable!(),
		}
	}
}

impl Instr for SimpleBranchInstr {
	fn block(&self) -> Rc<RefCell<Block>> {
		self.block.clone()
	}

	fn size(&self) -> u32 {
		self.size
	}

	fn ip(&self) -> u64 {
		self.ip
	}

	fn set_ip(&mut self, new_ip: u64) {
		self.ip = new_ip
	}

	fn orig_ip(&self) -> u64 {
		self.orig_ip
	}

	fn initialize(&mut self, block_encoder: &BlockEncoder) {
		self.target_instr = block_encoder.get_target(self, self.instruction.near_branch_target());
		let _ = self.try_optimize();
	}

	fn optimize(&mut self) -> bool {
		self.try_optimize()
	}

	fn encode(&mut self, block: &mut Block) -> Result<(ConstantOffsets, bool), String> {
		let mut instr;
		let mut size;
		let instr_len;
		match self.instr_kind {
			InstrKind::Unchanged | InstrKind::Short => {
				// Temp needed if rustc < 1.36.0 (2015 edition)
				let tmp = self.target_instr.address(self);
				self.instruction.set_near_branch64(tmp);
				match block.encoder.encode(&self.instruction, self.ip) {
					Err(err) => Err(InstrUtils::create_error_message(&err, &self.instruction)),
					Ok(_) => Ok((block.encoder.get_constant_offsets(), true)),
				}
			}

			InstrKind::Near => {
				// Code:
				//		brins tmp		; native_instruction_size
				//		jmp short skip	; 2
				//	tmp:
				//		jmp near target	; 3/5/5
				//	skip:

				instr = self.instruction;
				instr.set_code(self.native_code);
				instr.set_near_branch64(self.ip.wrapping_add(self.native_instruction_size as u64).wrapping_add(2));
				size = match block.encoder.encode(&instr, self.ip) {
					Err(err) => return Err(InstrUtils::create_error_message(&err, &self.instruction)),
					Ok(len) => len as u32,
				};

				instr = Instruction::default();
				instr.set_near_branch64(self.ip.wrapping_add(self.near_instruction_size as u64));
				let code_near = match block.encoder.bitness() {
					16 => {
						instr.set_code(Code::Jmp_rel8_16);
						instr.set_op0_kind(OpKind::NearBranch16);
						Code::Jmp_rel16
					}

					32 => {
						instr.set_code(Code::Jmp_rel8_32);
						instr.set_op0_kind(OpKind::NearBranch32);
						Code::Jmp_rel32_32
					}

					64 => {
						instr.set_code(Code::Jmp_rel8_64);
						instr.set_op0_kind(OpKind::NearBranch64);
						Code::Jmp_rel32_64
					}

					_ => unreachable!(),
				};
				instr_len = match block.encoder.encode(&instr, self.ip.wrapping_add(size as u64)) {
					Err(err) => return Err(InstrUtils::create_error_message(&err, &self.instruction)),
					Ok(len) => len as u32,
				};
				size += instr_len;

				instr.set_code(code_near);
				instr.set_near_branch64(self.target_instr.address(self));
				match block.encoder.encode(&instr, self.ip.wrapping_add(size as u64)) {
					Err(err) => Err(InstrUtils::create_error_message(&err, &self.instruction)),
					Ok(_) => Ok((ConstantOffsets::default(), false)),
				}
			}

			InstrKind::Long => {
				debug_assert_eq!(64, block.encoder.bitness());
				debug_assert!(self.pointer_data.is_some());
				let pointer_data = self.pointer_data.clone().unwrap();
				pointer_data.borrow_mut().data = self.target_instr.address(self);

				// Code:
				//		brins tmp		; native_instruction_size
				//		jmp short skip	; 2
				//	tmp:
				//		jmp [mem_loc]	; 6
				//	skip:

				instr = self.instruction;
				instr.set_code(self.native_code);
				instr.set_near_branch64(self.ip.wrapping_add(self.native_instruction_size as u64).wrapping_add(2));
				size = match block.encoder.encode(&instr, self.ip) {
					Err(err) => return Err(InstrUtils::create_error_message(&err, &self.instruction)),
					Ok(len) => len as u32,
				};

				instr = Instruction::default();
				instr.set_near_branch64(self.ip.wrapping_add(self.long_instruction_size as u64));
				match block.encoder.bitness() {
					16 => {
						instr.set_code(Code::Jmp_rel8_16);
						instr.set_op0_kind(OpKind::NearBranch16);
					}

					32 => {
						instr.set_code(Code::Jmp_rel8_32);
						instr.set_op0_kind(OpKind::NearBranch32);
					}

					64 => {
						instr.set_code(Code::Jmp_rel8_64);
						instr.set_op0_kind(OpKind::NearBranch64);
					}

					_ => unreachable!(),
				}
				instr_len = match block.encoder.encode(&instr, self.ip.wrapping_add(size as u64)) {
					Err(err) => return Err(InstrUtils::create_error_message(&err, &self.instruction)),
					Ok(len) => len as u32,
				};
				size += instr_len;

				match InstrUtils::encode_branch_to_pointer_data(
					block,
					false,
					self.ip.wrapping_add(size as u64),
					pointer_data,
					self.size.wrapping_sub(size),
				) {
					Ok(_) => Ok((ConstantOffsets::default(), false)),
					Err(err) => Err(InstrUtils::create_error_message(&err, &self.instruction)),
				}
			}

			InstrKind::Uninitialized => unreachable!(),
		}
	}
}
