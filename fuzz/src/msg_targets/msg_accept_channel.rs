// This file is auto-generated by gen_target.sh based on msg_target_template.txt
// To modify it, modify msg_target_template.txt and run gen_target.sh instead.

use lightning::ln::msgs;

use msg_targets::utils::VecWriter;
use utils::test_logger;

#[inline]
pub fn msg_accept_channel_test<Out: test_logger::Output>(data: &[u8], _out: Out) {
	test_msg!(msgs::AcceptChannel, data);
}

#[no_mangle]
pub extern "C" fn msg_accept_channel_run(data: *const u8, datalen: usize) {
	let data = unsafe { std::slice::from_raw_parts(data, datalen) };
	test_msg!(msgs::AcceptChannel, data);
}
