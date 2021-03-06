/*
 * Nibble - Concurrent Log-Structured Memory for Many-Core Key-Value Stores
 *
 * (c) 2017 Hewlett Packard Enterprise Development LP.
 *
 * This program is free software: you can redistribute it and/or modify it under the terms of the
 * GNU Lesser General Public License as published by the Free Software Foundation, either version 3
 * of the License, or (at your option) any later version. This program is distributed in the hope that
 * it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
 * FITNESS FOR A PARTICULAR PURPOSE.  See the GNU Lesser General Public License for more details.
 *
 * You should have received a copy of the GNU Lesser General Public License along with this program.
 * If not, see <http://www.gnu.org/licenses/>. As an exception, the copyright holders of this Library
 * grant you permission to (i) compile an Application with the Library, and (ii) distribute the Application
 * containing code generated by the Library and added to the Application during this compilation process
 * under terms of your choice, provided you also meet the terms and conditions of the Application license.
 */


#[macro_use]
extern crate kvs;

use std::ffi::CString;

use kvs::lsm::{self, LSM};
use kvs::numa::{self, NodeId};
use kvs::common::{self, Pointer};

static mut KVS: Pointer<LSM> = Pointer(0 as *const LSM);

#[no_mangle]
fn kvs_get() {
    println!("proxy invoked");
}

#[link(name = “otherprog”)]
extern {
    fn program_main(argc: i32, argv: **mut u8) -> i32;
}

fn main() {
    let kvs =
        Box::new(LSM::new(1usize<<35));
    unsafe {
        let p = Box::into_raw(kvs);
        KVS = Pointer(p);
    }

    for node in 0..numa::NODE_MAP.sockets() {
        kvs.enable_compaction(NodeId(node));
    }

    unsafe {
        let name: &'static str = "proxy";
        let argv = CString::new(name);
        program_main(1i32, argv.as_ptr()); // doesn’t return
    }
}
