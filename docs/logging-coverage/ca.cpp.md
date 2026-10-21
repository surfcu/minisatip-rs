# Logging coverage: `src/ca.cpp`

Sites in file order: every `LOG`, `LOGM`, `LOG_AND_RETURN` and `hexdump`/
`_hexdump` call. "Text" is the C++ format string, truncated to 60 characters.
`LOG` and `LOG_AND_RETURN` map to `ms_log::log!` (general), `LOGM` to
`ms_log::logm!` with `DEFAULT_LOG = LOG_DVBCA`, and `hexdump` to
`ms_log::hexdump!`. The ported Rust calls carry the C++ text byte for byte,
with printf conversion translated to the equivalent Rust formatting.

| C++ line | macro | text (first 60 chars) | Rust file:line or n/a: reason |
|---:|---|---|---|
| 285 | LOG | EVP_MD_CTX_new failed | n/a: CI+ signing not ported |
| 290 | LOG | EVP_DigestSignInit failed | n/a: CI+ signing not ported |
| 296 | LOG | set RSA_PKCS1_PSS_PADDING failed | n/a: CI+ signing not ported |
| 302 | LOG | set saltlen failed | n/a: CI+ signing not ported |
| 308 | LOG | set mgf1 md failed | n/a: CI+ signing not ported |
| 314 | LOG | EVP_DigestSign failed | n/a: CI+ signing not ported |
| 329 | LOG | EVP_PKEY_get1_RSA failed | n/a: CI+ signing not ported |
| 336 | LOG | RSA_padding_add_PKCS1_PSS failed | n/a: CI+ signing not ported |
| 342 | LOG | RSA_private_encrypt failed | n/a: CI+ signing not ported |
| 427 | LOG | RAND_bytes failed | n/a: CI+ nonce generation not ported |
| 452 | LOG | len > dest_len | n/a: CI+ DER building not ported |
| 500 | LOG | seems our rtc is wrong - ignore! | n/a: CI+ auth file not ported |
| 516 | LOG | can not open %s | n/a: CI+ auth file not ported |
| 522 | LOG | read error | n/a: CI+ auth file not ported |
| 536 | LOG | can not open %s | n/a: CI+ certificate not ported |
| 542 | LOG | can not read cert | n/a: CI+ certificate not ported |
| 563 | LOG | %s | n/a: CI+ certificate not ported |
| 582 | LOG | can not create cert_store | n/a: CI+ certificate not ported |
| 587 | LOG | load of first certificate (root_ca) failed | n/a: CI+ certificate not ported |
| 596 | LOG | can not open certificate %s | n/a: CI+ certificate not ported |
| 601 | LOG | can not vaildate certificate | n/a: CI+ certificate not ported |
| 608 | LOG | load of certificate failed | n/a: CI+ certificate not ported |
| 622 | LOG | can not read certificate | n/a: CI+ certificate not ported |
| 627 | LOG | can not vaildate certificate | n/a: CI+ certificate not ported |
| 663 | LOG | fixme: unsupported chars in hostid | n/a: CI+ hostid not ported |
| 687 | LOG | Could not open certificate file %s | n/a: CI+ certificate dump not ported |
| 690 | LOG | ######################################################### | n/a: CI+ certificate dump not ported |
| 705 | LOG | %s | n/a: CI+ certificate dump not ported |
| 713 | LOG | ######################################################### | n/a: CI+ certificate dump not ported |
| 722 | LOG | Requesting CA %d close | crates/ms-ca-drivers/src/en50221/device.rs:252 |
| 896 | LOGM | %s: skipping pmt %d (ad %d) pid %d from CAPMT | crates/ms-ca/src/capmt.rs:216 |
| 903 | LOG | %s: truncating PMT %d stream list at %d bytes | crates/ms-ca/src/capmt.rs:229 |
| 934 | LOG_AND_RETURN | %s: PMT %d not found | crates/ms-ca/src/capmt.rs:277 |
| 990 | LOG_AND_RETURN | create_capmt failed | crates/ms-ca/src/device.rs:235 |
| 994 | LOG | CA %d: CAPMT for PMTs %s unchanged (%08X), not re-sending | crates/ms-ca/src/device.rs:240 |
| 1009 | LOG_AND_RETURN | create_capmt failed | crates/ms-ca/src/device.rs:251 |
| 1015 | LOG_AND_RETURN | Unable to find CA session for device %d | crates/ms-ca/src/device.rs:256 |
| 1017 | hexdump | CAPMT:  | crates/ms-ca/src/device.rs:259 |
| 1116 | LOG | CA %d all channels used %d, multiple allowed %d | crates/ms-ca/src/device.rs:119 |
| 1119 | LOG | CAPMT %d pmts %s | crates/ms-ca/src/device.rs:126 |
| 1139 | LOG | %s: %s -> %c | n/a: enigma CI sysfs routing not ported |
| 1145 | LOG | %s open failed, %s | n/a: enigma CI sysfs routing not ported |
| 1161 | LOG | %s: %s -> %s | n/a: enigma CI sysfs routing not ported |
| 1167 | LOG | %s:  open failed, %s | n/a: enigma CI sysfs routing not ported |
| 1194 | LOG | Found CA %d for PMT %d | n/a: enigma DVB-CI lookup not ported |
| 1258 | LOG_AND_RETURN | CAM not yet initialized | crates/ms-ca/src/device.rs:284 |
| 1268 | LOG_AND_RETURN | No free slots to add PMT %d to CA %d | crates/ms-ca/src/device.rs:289 |
| 1274 | LOG_AND_RETURN | First PMT not found from capmt %d | crates/ms-ca/src/device.rs:296 |
| 1300 | LOG | PMT CA %d pmt %d pid %u (%s) ver %u sid %X (%u), enabled_pmts %d,  | crates/ms-ca/src/device.rs:331 |
| 1310 | LOG_AND_RETURN | send_capmt failed | crates/ms-ca/src/device.rs:359 |
| 1367 | LOG_AND_RETURN | CAPMT not found for pmt %d | crates/ms-ca/src/device.rs:368 |
| 1381 | LOG | %s: send_capmt failed releasing pmt %d | crates/ms-ca/src/device.rs:389 |
| 1388 | LOG | PMT CA %d DEL pmt %d pid %u, sid %u (%u), ver %d, %s name: %s | crates/ms-ca/src/device.rs:399 |
| 1397 | LOG | setting CI+ CAM data rate to %s Mbps | crates/ms-ca-drivers/src/en50221/apps.rs:241 |
| 1407 | LOG | element_get: invalid id | n/a: CI+ element store not ported |
| 1440 | LOG | size %d of datatype_id %d doesn't match | n/a: CI+ element store not ported |
| 1450 | LOGM | _element_set_ stored %d with len %d | n/a: CI+ element store not ported |
| 1462 | LOG | can not get data in DER format | n/a: CI+ element store not ported |
| 1467 | LOG | can not store element (%d) | n/a: CI+ element store not ported |
| 1485 | LOG | wrong datatype_id for hostid | n/a: CI+ element store not ported |
| 1504 | LOG | malformed hostid | n/a: CI+ element store not ported |
| 1507 | LOG | %sID: %s | n/a: CI+ element store not ported |
| 1512 | LOG | can not set hostid | n/a: CI+ element store not ported |
| 1536 | LOG | element_get_buf: datatype %d not valid | n/a: CI+ element store not ported |
| 1541 | LOG | element_get_buf: datatype %d doesn't exist | n/a: CI+ element store not ported |
| 1556 | LOG | can not get element %d | n/a: CI+ element store not ported |
| 1575 | LOG | element_get_ptr: datatype %u not valid | n/a: CI+ element store not ported |
| 1580 | LOG | element_get_ptr: datatype %u doesn't exist | n/a: CI+ element store not ported |
| 1619 | LOG | cannot open %s | n/a: CI+ authdata store not ported |
| 1625 | LOG | cannot read auth_data | n/a: CI+ authdata store not ported |
| 1659 | LOG | data already stored | n/a: CI+ authdata store not ported |
| 1664 | LOG | got %d entries | n/a: CI+ authdata store not ported |
| 1670 | LOG | cannot open %s for writing - authdata not stored | n/a: CI+ authdata store not ported |
| 1676 | LOG | error in write | n/a: CI+ authdata store not ported |
| 1682 | LOG | error in write | n/a: CI+ authdata store not ported |
| 1688 | LOG | error in write | n/a: CI+ authdata store not ported |
| 1700 | LOG | error in write | n/a: CI+ authdata store not ported |
| 1722 | LOG | can not set status in elements | n/a: CI+ authdata store not ported |
| 1728 | LOG | uri version bitmask set to '%d' | n/a: CI+ authdata store not ported |
| 1730 | LOG | can not set uri_versions in elements | n/a: CI+ authdata store not ported |
| 1744 | LOG | can not set AKH in elements | n/a: CI+ authdata store not ported |
| 1748 | LOG | can not set host_id elements | n/a: CI+ authdata store not ported |
| 1753 | LOG | can not set csuv elements | n/a: CI+ authdata store not ported |
| 1765 | LOG | auth too short | n/a: CI+ authdata store not ported |
| 1775 | LOG | signature wrong | n/a: CI+ authdata store not ported |
| 1779 | LOG | auth ok! | n/a: CI+ authdata store not ported |
| 1864 | LOG | cannot read/verify DER cert | n/a: CI+ auth not ported |
| 1879 | LOG | CICAM brand cert invalid | n/a: CI+ auth not ported |
| 1885 | LOG | CICAM device cert invalid | n/a: CI+ auth not ported |
| 1895 | LOG | CI%d EXTRACTING %s | n/a: CI+ auth not ported |
| 1899 | LOG_AND_RETURN | opening %s for writing failed | n/a: CI+ auth not ported |
| 1903 | LOG | write cert failed | n/a: CI+ auth not ported |
| 1911 | LOG | can not import brand cert | n/a: CI+ auth not ported |
| 1917 | LOG | can not import device cert | n/a: CI+ auth not ported |
| 1923 | LOG | can not set cicam_id in elements | n/a: CI+ auth not ported |
| 1950 | LOG | checking ... | n/a: CI+ auth not ported |
| 1954 | LOG | auth nonce invalid | n/a: CI+ auth not ported |
| 1960 | LOG | cicam id invalid | n/a: CI+ auth not ported |
| 1966 | LOG | dphm invalid | n/a: CI+ auth not ported |
| 1972 | LOG | signature B invalid | n/a: CI+ auth not ported |
| 1986 | LOG | writing authdata ... | n/a: CI+ auth not ported |
| 1997 | LOG | .... rechecking ... | n/a: CI+ auth not ported |
| 2014 | LOG | can not check loader certificates | n/a: CI+ auth not ported |
| 2020 | LOG | can not store cert in elements | n/a: CI+ auth not ported |
| 2023 | LOG | can not store cert in elements | n/a: CI+ auth not ported |
| 2026 | LOG | can not set hostid in elements | n/a: CI+ auth not ported |
| 2030 | LOG | can not read private key | n/a: CI+ auth not ported |
| 2093 | LOG | key precursor invalid | n/a: CI+ key register not ported |
| 2099 | LOG | slot(key register) invalid | n/a: CI+ key register not ported |
| 2112 | LOGM | === descrambler_set_key === adapter = CA%i key regiser (0-even,  | n/a: CI+ key register not ported |
| 2124 | LOG | received from CI+ CAM %d: %s | n/a: CI+ key register not ported |
| 2183 | LOGM | not need to be handled id %d | n/a: CI+ content control not ported |
| 2187 | LOG | unhandled id %d | n/a: CI+ content control not ported |
| 2205 | LOG | cannot set AKH in elements | n/a: CI+ content control not ported |
| 2207 | LOG | cannot set host_id in elements | n/a: CI+ content control not ported |
| 2232 | LOGM | set element(dt_id) %d dt_len = %i | n/a: CI+ content control not ported |
| 2257 | LOGM | req element %d | n/a: CI+ content control not ported |
| 2263 | LOG | cannot get element %d | n/a: CI+ content control not ported |
| 2279 | LOGM | SEND ------------ CC_OPEN_CNF-----------  | n/a: CI+ content control not ported |
| 2288 | LOG | ______________________ci_ccmgr_cc_sac_send______________________ | n/a: CI+ content control not ported |
| 2326 | LOG | check_auth of message failed | n/a: CI+ content control not ported |
| 2333 | LOGM | serial sac data req: %d | n/a: CI+ content control not ported |
| 2359 | LOG | cannot req data | n/a: CI+ content control not ported |
| 2364 | LOGM | SEND ------------ CC_SAC_DATA_CNF-----------  | n/a: CI+ content control not ported |
| 2388 | LOG | SEND ------------ CC_SAC_SYNC_CNF-----------  | n/a: CI+ content control not ported |
| 2395 | LOGM | SEND ------------ CC_SYNC_CNF-----------  | n/a: CI+ content control not ported |
| 2432 | LOG | cannot req data | n/a: CI+ content control not ported |
| 2438 | LOGM | SEND ------------ CC_DATA_CNF-----------  | n/a: CI+ content control not ported |
| 2458 | LOG | RECV ciplus cc msg CAM%i, session_num %u, tag %x len %i dt_id %d | n/a: CI+ content control not ported |
| 2481 | LOG | !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!! unknown cc  | n/a: CI+ content control not ported |
| 2508 | LOG | country answered with '%c%c%c' | crates/ms-ca-drivers/src/en50221/apps.rs:145 |
| 2516 | LOG | language answered with '%c%c%c' | crates/ms-ca-drivers/src/en50221/apps.rs:149 |
| 2523 | LOG | unknown host lac apdu tag %02x | crates/ms-ca-drivers/src/en50221/apps.rs:152 |
| 2560 | hexdump | CIPLUS_APP_OPRF_handler | crates/ms-ca-drivers/src/en50221/apps.rs:360 |
| 2594 | LOG | Received %s APDU: \n | crates/ms-ca-drivers/src/en50221/apps.rs:364 |
| 2605 | LOG | Initializing operator_info_req since profile is not initialized yet | crates/ms-ca-drivers/src/en50221/apps.rs:378 |
| 2621 | LOG | Received operator_info APDU: \n | crates/ms-ca-drivers/src/en50221/apps.rs:389 |
| 2639 | LOG | Received operator_tune APDU with %d descriptor bytes | crates/ms-ca-drivers/src/en50221/apps.rs:402 |
| 2650 | LOG | CAM wants to tune to the following transponder:  | n/a: operator tune descriptor parsing not ported |
| 2664 | LOG | unknown OPRF apdu tag %03x | crates/ms-ca-drivers/src/en50221/apps.rs:409 |
| 2675 | LOG | CAM_fw_upgrade_receive | crates/ms-ca-drivers/src/en50221/apps.rs:155 |
| 2680 | LOG | CI+ CA%i Firmware Upgrade Command detected...  | crates/ms-ca-drivers/src/en50221/apps.rs:158 |
| 2687 | LOG | CI+ CA%i Firmware Upgrade Progress %i percents | crates/ms-ca-drivers/src/en50221/apps.rs:163 |
| 2692 | LOG | CI+ CA%i Firmware Upgrade Complete (reset status %i) | crates/ms-ca-drivers/src/en50221/apps.rs:172 |
| 2700 | LOG | unknown fw upgrade apdu tag %06X | crates/ms-ca-drivers/src/en50221/apps.rs:182 |
| 2724 | LOG | Sending time to CA %d to session %d, response interval: %jd | crates/ms-ca-drivers/src/en50221/apps.rs:288 |
| 2733 | LOG | Got unhandled resource %06X | crates/ms-ca-drivers/src/en50221/apps.rs:186 |
| 2738 | LOG | RM Created | crates/ms-ca-drivers/src/en50221/apps.rs:83 |
| 2756 | LOG | unexpected tag in ResourceManagerHandle (0x%x) | crates/ms-ca-drivers/src/en50221/apps.rs:204 |
| 2763 | LOG | %s: CAM requested version %d of the Application Information resource | crates/ms-ca-drivers/src/en50221/apps.rs:91 |
| 2776 | LOG | RECV ciplus AI msg CA %u, session_num %u, resource_id %x | crates/ms-ca-drivers/src/en50221/apps.rs:210 |
| 2782 | hexdump | CIPLUS_TAG_APP_INFO | crates/ms-ca-drivers/src/en50221/apps.rs:218 |
| 2784 | LOG | CA %d: application info too short (%d) | crates/ms-ca-drivers/src/en50221/apps.rs:220 |
| 2787 | LOG |   Application type: %02x | crates/ms-ca-drivers/src/en50221/apps.rs:226 |
| 2788 | LOG |   Application manufacturer: %04X | crates/ms-ca-drivers/src/en50221/apps.rs:227 |
| 2789 | LOG |   Manufacturer code: %04X | crates/ms-ca-drivers/src/en50221/apps.rs:231 |
| 2797 | LOG |   CAM Name: %s (%d) | crates/ms-ca-drivers/src/en50221/apps.rs:239 |
| 2806 | hexdump | CIPLUS_TAG_CICAM_RESET | crates/ms-ca-drivers/src/en50221/apps.rs:247 |
| 2807 | LOG | CA %d Reset requested | crates/ms-ca-drivers/src/en50221/apps.rs:248 |
| 2813 | LOG | unexpected tag in %s (0x%x) | crates/ms-ca-drivers/src/en50221/apps.rs:251 |
| 2825 | LOG | Closed the CA session for CA %d | crates/ms-ca-drivers/src/en50221/apps.rs:119 |
| 2851 | LOG | CA%d: CAM reported more CAIDs than MAX_CAID limit | crates/ms-ca-drivers/src/en50221/apps.rs:264 |
| 2862 | LOG |    Supported CA ID: %04X for CA%d | crates/ms-ca-drivers/src/en50221/apps.rs:277 |
| 2868 | LOG | %s: unexpected tag (0x%x) | crates/ms-ca-drivers/src/en50221/apps.rs:257 |
| 2883 | LOG | unexpected tag in %s (0x%x) | crates/ms-ca-drivers/src/en50221/apps.rs:138 |
| 2913 | hexdump | Got MMI:  | crates/ms-ca-drivers/src/en50221/apps.rs:300 |
| 2920 | LOG | mmi display ctl cb received for CA %u session_num %u  | crates/ms-ca-drivers/src/en50221/apps.rs:305 |
| 2940 | LOG | MMI enquiry from CAM in CA %u:  %s (%s%u digits) | crates/ms-ca-drivers/src/en50221/apps.rs:321 |
| 2944 | LOG | answering to PIN enquiry | crates/ms-ca-drivers/src/en50221/apps.rs:334 |
| 2965 | LOG | CA %d ======= MMI menu ======= | crates/ms-ca-drivers/src/en50221/apps.rs:36 |
| 2970 | DEBUGM | [%d] text tag: %02x %02x %02x | crates/ms-ca-drivers/src/en50221/apps.rs:41 |
| 2975 | LOG | CA %d asn_1_decode failed: %02X %02X %02X | crates/ms-ca-drivers/src/en50221/apps.rs:51 |
| 2980 | DEBUGM | [%d] %d bytes text | crates/ms-ca-drivers/src/en50221/apps.rs:62 |
| 2986 | LOG | [UI %d] %s | crates/ms-ca-drivers/src/en50221/apps.rs:66 |
| 2989 | LOG | ====================== | crates/ms-ca-drivers/src/en50221/apps.rs:70 |
| 2997 | LOG | %s: CA %d unknown tag resource %06X | crates/ms-ca-drivers/src/en50221/apps.rs:349 |
| 3007 | LOG | %s: Sending language to the CAM | crates/ms-ca-drivers/src/en50221/apps.rs:108 |
| 3083 | LOG_AND_RETURN | CA %d: resource %06X is not supported | crates/ms-ca-drivers/src/en50221/device.rs:614 |
| 3089 | LOG_AND_RETURN | Could not allocate a new session id for CA %d | crates/ms-ca-drivers/src/en50221/device.rs:618 |
| 3109 | LOG | Populating %s resources | crates/ms-ca-drivers/src/en50221/apps.rs:197 |
| 3172 | LOG | incomplete write to CA %d fd %d, expected %d got %d, errno %d  | crates/ms-ca-drivers/src/en50221/device.rs:363 |
| 3229 | hexdump | WRITE TPDU:  | crates/ms-ca-drivers/src/en50221/device.rs:413 |
| 3238 | LOG | incomplete write to CA %d fd %d, expected %d got %d, errno %d %s | crates/ms-ca-drivers/src/en50221/device.rs:375 |
| 3333 | DEBUGM | %s: CA %d, session %d, name %s, write tag %02X, data length %ld | crates/ms-ca-drivers/src/en50221/device.rs:433 |
| 3339 | hexdump | WRITE SPDU:  | crates/ms-ca-drivers/src/en50221/device.rs:442 |
| 3355 | LOG | ca_write_apdu: CA %d, session %d, name %s, write tag %06X, data  | crates/ms-ca-drivers/src/en50221/device.rs:456 |
| 3389 | hexdump | READ TPDU:  | crates/ms-ca-drivers/src/en50221/device.rs:516 |
| 3400 | LOG | Received data with invalid asn from module on device %d | crates/ms-ca-drivers/src/en50221/device.rs:522 |
| 3402 | _hexdump | INVALID LENGTH:  | crates/ms-ca-drivers/src/en50221/device.rs:526 |
| 3408 | LOG | Received data with invalid length from module on device  | crates/ms-ca-drivers/src/en50221/device.rs:530 |
| 3411 | _hexdump | INVALID LENGTH:  | crates/ms-ca-drivers/src/en50221/device.rs:534 |
| 3416 | LOG | Received data for CA %d larger than socket buffer (len  | crates/ms-ca-drivers/src/en50221/device.rs:539 |
| 3428 | DEBUGM | Got CTC Replay (CA %d) | crates/ms-ca-drivers/src/en50221/device.rs:549 |
| 3435 | LOG | Got "Delete Transport Connection" from module  | crates/ms-ca-drivers/src/en50221/device.rs:556 |
| 3440 | LOG | Got "Delete Transport Connection Replay" from  | crates/ms-ca-drivers/src/en50221/device.rs:560 |
| 3444 | LOG | Got "Request Transport Connection" from Module  | crates/ms-ca-drivers/src/en50221/device.rs:563 |
| 3460 | LOG | unhandled tpdu_tag 0x%0x\n | crates/ms-ca-drivers/src/en50221/device.rs:571 |
| 3479 | hexdump | APDU | crates/ms-ca-drivers/src/en50221/device.rs:731 |
| 3484 | LOG | Invalid APDU length %02X %02X %02X | crates/ms-ca-drivers/src/en50221/device.rs:734 |
| 3491 | LOG | %s: CA %d, session %d, name %s, read tag %06X, data length %d | crates/ms-ca-drivers/src/en50221/device.rs:750 |
| 3496 | hexdump | data:  | crates/ms-ca-drivers/src/en50221/device.rs:759 |
| 3520 | hexdump | CAREAD | crates/ms-ca-drivers/src/en50221/device.rs:637 |
| 3532 | LOG | Invalid SPDU length %02X %02X %02X | crates/ms-ca-drivers/src/en50221/device.rs:646 |
| 3543 | LOG | Found session_number %d for resource %06X, name %s, status %02X | crates/ms-ca-drivers/src/en50221/device.rs:667 |
| 3560 | LOG | CA %d: bad session %d, dropping | crates/ms-ca-drivers/src/en50221/device.rs:687 |
| 3564 | LOG | Received close session %s | crates/ms-ca-drivers/src/en50221/device.rs:693 |
| 3584 | DEBUGM | got ST_SESSION_NUMBER for session_number %d | crates/ms-ca-drivers/src/en50221/device.rs:713 |
| 3586 | LOG | CA %d: bad session %d, dropping | crates/ms-ca-drivers/src/en50221/device.rs:715 |
| 3591 | LOG | CA %d: session %d is not registered | crates/ms-ca-drivers/src/en50221/device.rs:722 |
| 3615 | LOG | closing CA device %d, fd %d | crates/ms-ca-drivers/src/en50221/device.rs:290 |
| 3665 | LOG | CA %d: command %06X unanswered after %jd ms, resetting | crates/ms-ca-drivers/src/en50221/device.rs:801 |
| 3668 | LOG | CA %d: initialization stalled after %jd ms, resetting | crates/ms-ca-drivers/src/en50221/device.rs:808 |
| 3714 | LOG_AND_RETURN | %s: sockets_add failed | n/a: no Rust socket layer in this path |
| 3724 | LOG | initializing CA %d, fd %d sock %d | crates/ms-ca-drivers/src/en50221/device.rs:342 |
| 3763 | LOG | CA %d: no module present, not retrying reset | crates/ms-ca-drivers/src/en50221/link.rs:87 |
| 3767 | LOG | CA %d: module not ready after reset %d/%d, retrying | crates/ms-ca-drivers/src/en50221/link.rs:91 |
| 3782 | LOG_AND_RETURN | %s: io error on ca %d | crates/ms-ca-drivers/src/en50221/device.rs:313 |
| 3784 | LOG | initializing CA %d, fd %d type %d flags 0x%x, after %jd ms | crates/ms-ca-drivers/src/en50221/device.rs:316 |
| 3788 | LOG | incompatible CA interface | crates/ms-ca-drivers/src/en50221/device.rs:325 |
| 3793 | LOG | CA module not present or not ready | crates/ms-ca-drivers/src/en50221/device.rs:329 |
| 3801 | LOG | %s:8 sockets_add failed | n/a: no Rust socket layer in this path |
| 3822 | LOG_AND_RETURN | Could not allocate memory for CA device | n/a: no Rust malloc fallback |
| 3835 | LOG | Failed to set the handle %d as non blocking | n/a: no Rust fcntl step |
| 3842 | LOG | Failed to set the handle %d as blocking | n/a: no Rust fcntl step |
| 3921 | LOG_AND_RETURN | No CA device detected on adapter %d: enabling already registered CAs | crates/ms-ca-drivers/src/en50221/registry.rs:121 |
| 3926 | LOG_AND_RETURN | No CA device detected on adapter %d: file %s | crates/ms-ca-drivers/src/en50221/registry.rs:128 |
| 3935 | LOG_AND_RETURN | Could not allocate memory for CA device %d | n/a: no Rust malloc fallback |
| 3972 | LOG | CA %d: no module in slot, deferring init | crates/ms-ca-drivers/src/en50221/registry.rs:158 |
| 4023 | LOG | PMT %d, pid %d was added, invalidating pmt | n/a: DvbcaSink has no add_pid hook |
| 4050 | LOG | Trying to reconnect to CA %d | crates/ms-ca-drivers/src/en50221/registry.rs:203 |
| 4120 | LOG | Forcing CA %d to CI | crates/ms-config/src/parse.rs:1419 |
| 4146 | LOG | Setting CA %d pin to %s | crates/ms-config/src/parse.rs:1407 |
| 4185 | LOG | Forcing CA %d to use maximum %d CAPMTs with %d PMTs each | crates/ms-config/src/parse.rs:1441 |
| 4198 | LOG | Forcing CA %d to use CAID %04X | crates/ms-config/src/parse.rs:1455 |
| 4200 | LOG | CA %d: reached maximum CAID limit (%d), ignoring CAID  | crates/ms-config/src/parse.rs:1458 |

Summary: ported 117 / not applicable 118 (235 sites; the 118 not applicable
are the CI+ crypto, certificate, element/authdata, key-register and
content-control code, the enigma CI sysfs routing, the C++ socket/malloc/fcntl
failure paths and `dvbca_add_pid`, none of which have a Rust code point).

Deviations are listed in the run report as `DEV-???:`; no Rust code logs more
often than the corresponding C++ site.
