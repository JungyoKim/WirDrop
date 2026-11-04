# Third-party notices for wirdrop-radio

## Linux rtw88

The RTL8822B bring-up in this crate (`src/rtl8822b/`, `src/dm.rs`, the generated tables, power
sequences and register definitions in `src/generated/`, and the descriptor layouts in
`src/radio.rs`) is ported from the Linux kernel driver rtw88
(drivers/net/wireless/realtek/rtw88), which is dual-licensed GPL-2.0 OR BSD-3-Clause. It is used
here under the BSD-3-Clause license:

Copyright(c) 2018-2019 Realtek Corporation

Redistribution and use in source and binary forms, with or without modification, are permitted
provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this list of conditions
   and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice, this list of
   conditions and the following disclaimer in the documentation and/or other materials provided
   with the distribution.
3. Neither the name of the copyright holder nor the names of its contributors may be used to
   endorse or promote products derived from this software without specific prior written
   permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR
CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER
IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT
OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

## Linux rtw89

The RTL8912AU / RTL8922A bring-up in this crate (`src/rtl8922a/`: power sequences, firmware
download, MAC/BB/RF initialisation, channel switching, RF calibration requests, transmit power
tables, TX/RX descriptors, address and Block Ack CAM handling, and the register definitions
generated into `src/rtl8922a/generated/` by `tools/gen_rtw89.py`) is ported from the Linux
kernel driver rtw89 (drivers/net/wireless/realtek/rtw89), which is dual-licensed GPL-2.0 OR
BSD-3-Clause. It is used here under the BSD-3-Clause license, with the same terms as above:

Copyright(c) 2019-2020, 2023, 2025 Realtek Corporation

Redistribution and use in source and binary forms, with or without modification, are permitted
provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this list of conditions
   and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright notice, this list of
   conditions and the following disclaimer in the documentation and/or other materials provided
   with the distribution.
3. Neither the name of the copyright holder nor the names of its contributors may be used to
   endorse or promote products derived from this software without specific prior written
   permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR
CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER
IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT
OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

## Realtek firmware

`firmware/rtw8822b_fw.bin` (RTL8822B, from linux-firmware rtw88/) and
`firmware/rtw8922a_fw-4.bin` (RTL8922A, version 0.35.80.3, from linux-firmware rtw89/) are
Realtek's firmware, shipped unmodified and embedded in usbfwd.exe. Their license is in
`firmware/LICENCE.rtlwifi_firmware.txt` (Copyright (c) 2010, Realtek Semiconductor Corporation;
redistributable in binary form without modification); linux-firmware's WHENCE lists both under it.

## Everything else

The rest of this crate (the WinUSB transport, the usbfwd relay, and the glue) is WirDrop's own
code under the MIT license (../../LICENSE).

