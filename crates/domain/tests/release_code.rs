use bbt_domain::release_code;

#[test]
fn finds_the_code_in_real_titles() {
    for (title, code) in [
        ("[Reducing Mosaic]MIDA-859", "MIDA-859"),
        ("[Demosaic - 1080P] - [Mida] MIDA-859", "MIDA-859"),
        ("ABF-399-LTX [ลดเซนเซอร์ด้วย AI]", "ABF-399"),
        ("[4K 60fps] - [Imai Natsume] CAWB-078", "CAWB-078"),
        ("cawb-078 lower case", "CAWB-078"),
        ("CAWB-0078", "CAWB-078"),
        ("PRED-909 คุณเองก็สามารถพัฒนาทักษะการเล้าโลมได้เช่นกัน", "PRED-909"),
    ] {
        assert_eq!(release_code(title).as_deref(), Some(code), "{title}");
    }
}

#[test]
fn ignores_things_that_are_not_codes() {
    for title in [
        "Natasha Rios - PART 1 52 videos 2021-2026",
        "คลิปหลุด โอนลี่แฟน Robin Hood888 น้องน้ำหวาน",
        "MIDA ขนาด 720P ประจำวันที่ 13/11/2 MIDA-(830,847,855)",
        "Asian mix Vol 171",
        "X-1000 one letter",
        "ABCDEFG-123 seven letters",
        "ABC-12 two digits",
        "ABC-1234X glued",
        "",
    ] {
        assert_eq!(release_code(title), None, "{title}");
    }
}
