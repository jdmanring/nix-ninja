//! The pkg-config 0.29.2 placeholder object, 2026-10-09: `gcc -g3 -O2
//! -gz=zlib -c` on a source whose only mention of the store path is a
//! macro, so the placeholder lives ONLY inside the zlib-compressed
//! `.debug_macro` section. Embedded as zlib+base64 rather than a sibling
//! `.o` because the driver's flake `src` globs `.rs` alone, so an `.o`
//! beside this file is invisible to the nix build.
#![allow(dead_code)]

const OBJECT_B64: &str = "eNql2wk4Vd8WAPB7zfMsJCFlyDxPlTIkY4QMERfXGDIlMyFTRVJKSZGUFKU0SKZKiUqShNAkRTRISF5l+af1z+t937t97nV+\
zjl7n73XXnsfTtF6xqspiETCzItIWEH4tfXr9Vrz1/cr4V2AsODnvszfv1h+HksgsH7/Yvv+dWnp97c6wj/ncvjn/NOvrYdS\
XZ2cnMhOLwjrmHYIae9rd9hxgChQevT9geo3B2x0zp4gHaprrM0+YF1bTSA436NcQ2QnLmHhZEwnMu4m0lAK8guyEzlo2XhC\
eQgEChmCFh8liwajJqM6oxifAeePH63kDeMjECgXExgZVzBS0qHy5f5VnxbS9/o4E4gsNK7VN2MECASGJMJyagLFj5/S/nij\
JBApCRQ/v6Wi/nkc1a/jd9Hj8w1el2cnEOgIRx0HHNvL4jmZjE3qy306pi7q2dKOURMI1Jw/9qMW/fku9vP9Z6Wo5X++r/j9\
fLb/qi8j+Uf79TjJ0TzYsWNH1djWVVwiSbTz0t2dYyhEhQ7mHiBSy51jkN9ca0VXliJ3rqFMXuEsMZiCkENBsefXeezk8Hlz\
7GwDO9Q4Pq81J298ZmggzLKtp27Z65hNj9bVJ7kWDzBK+nPVc9bfvmlwOiRqUsU35lhEqFfGfp6YdzfbHKN3lgUrZrsoaG/a\
JuYRz9nIsSuj+UtHSffFRK0SlouJuW1Zw28Tc7Mcv13t29T42iNCr2cq+tvXvlfjzz2u1oj5bOB2Hzz/tDgltCL7XceWzE73\
3BHWTod9yvmDY+e9prwYzC++7Z+/cNVmd+FXmwu93N42+SpLd1gJDy3es3h8lbOxmq3Up0I2i7Sjx7gnpJdNhhmvHRHY1ip0\
I8y2OHW8kk8nTaawIFef/vh4wKjmYs0L9+rqOorEVb3y9sRbyDE3yL30zDYz9MhwVbqxzEDtTmCKQhBvfsTt15U5t69pPWuP\
8N1pmVrkW/hWb0Os+hsHkacmd6qFah4771h34cON4viKB6Y3DUsco8e/TTxv6s00DcguzWzOtIiXMJIwpovqs92l/pxXJKO3\
br+TXcMI99uwDUNhUee0Cspeel0500G9tVzg8lju2u7k8rCqwxc/NnlpdtFXpqT2l7/0IrrFapqneC5ZrVtyMv1uDnfqFolJ\
+/B7a3TaJ8LEM2tySjppbhnTJgSz7+fIuVl+KHdtCBtn8AbfV1NeWZrCq9U+bwsi7XOXEksyuyu3ypOGxMLSKC7Kt8pW2yBb\
4Gl6vCz9NWvipN9Z23Iep+L1pm26TLVXW2XkcvWWJY56Hq6dGHaQC69tkWqidzPerr+vp46foUqm10p9KFgxi/HJwDGrnJNx\
fhn0JnL18Qqr1eJ51wbuesHEnni7hevsgkI5K3WLrVWxfNnD+54OqH0MtAjLPZTVc1p9U00O71mlJY1+LNxmW7f5Z7vKLU2s\
2rzNqWVJ6Db6u2arhSI46BSTROQ49nyOoVveEm3PlTq+4Fr1wM0UCu6aFLa76mSuFbUkaduMuC7/wwHbQg+LbL9FMm4pUGa7\
WneV84VjA/2DmINd2gNCmXftzm7kYljTc+pW/PWIsjNOmk1vtbdLXC0spGIXXe5+XOQmk/k58cqRVyWya/Wz8rW+8Sp/XqrK\
48M0XpnOUZm8S8hhA4EiOfROhlJT5BGd+DdtTR0Kzt1+1PaXJj5M9pU0jwzKjvZU8zhRs4myy5QmnG9z28Aj6Sk0aX/SSdtv\
8bsaqepFSUvfuORxRF/4WLSwIPbyPdWKJ7qFZ9MO9DeNdpxqHuqoeHuqo05M0cUz6eX5B1v9F5i/cOFgrz6zpX1cMstxxVem\
lRrlAc83xt07OKrxXLWGauH+VwOR0sYD50X49cjmbaVim4RsP/bLCXPWyhk6BXLGb87Yo0Ti/XZHIckp6ZJUgj29OWN4xbLL\
l69NRg7J+6idveV356urSuIWG9pbW1nrZV6QGcdcRivOjWw9Vl1axPOx/GSW9I7iqjcDonvIPfPGNg9fu+oxeuhxaqgFD5vD\
h1jDgv69vJXbhnaNSfYt4tzg2TMuItM1r1Z397k3w+XpbMsPlb3cLnCpzG6Q6EvwKWq4GdHCKtMXqcjD+SSgm+Tbu4T32B2j\
xtNZeVcP7Y4YXWW5MMmnqqD4o9S5InrJgNLoTJmRaKkXnlF5tS9zHlslbONwUmHrLZiK2u/Xqzywptlc8HPd+OjgSCRbL+t9\
GirWyEuVE0M1NSuXUdLbvH74ips7UoruWvVqhjfR9oIRblbNQ0LUt9sbB8g53vS0hdp5cYdSqiu4DmWbCdEaaCvoht48cIw6\
otySxUOj0/N+ScPVp8YBNQWPJXVE1hwtkE+OoHhXcTL7q9L5TS49vukqOhWdixfRpqb5U15QFcxX4iUZrF5DEUFwFuh3ktUM\
v5fGWdjpK+VJ9Xb7sjT521cFycvy9nzeJGck+r58Z1nOjpyuIPMBnUcX0jwbWjzP9ya5LZSPLFjow6VstynbVqKTRUXCieDn\
2sk6UHKufuI21fGkAL6Lh1yyzfc59SQrEZctYlqlnBEoxWmR0nDiyxshz+fvNT/YV8+TTtWnIgoHjZ1130gZQvoWHLV3oiBa\
p009rDMio16L4lqR12Eh9SK1VI78RqrdIxRPnudx0PRnWAgIE5em7xGJd4qP2R4eXWs470ZA/4kTzP5T0s47mvWCWMtLNPaW\
GxdndWsJPPEbrjcJHx2sazK5WrTwvNfS1znp7odJ2/0zTAP2EfqO2+7LWHDLSCOdj8XlzFGfeQGWC2ptmJxrGq9YqWVa79rJ\
1UrzTr5B6R2ZnnHLme6H7fH5NaE3AhfTRAfPb6cwlyhMKbJsKfA4usVXV9WnItngWIrwZ9vOu65BVdab1NS3dImEJ7uFaxzY\
QjQu7GHtXlGTSvJnI2m9rxbMN3pn7bzo7oMXvsube6vmBw527tXTaCwrq4jwD8l8VP9l6hZVZifHeMJmzYZ8Q10PPdtjolxy\
adoNNKzaytmbs1dFZfTUs1My0X7SaYw5v3Z03rllG5crlVJfWekeeiXJLKxBSch3mXbSiRQGh08LZT0MtWrcj2/aby45cnzL\
kjcfm2rdr7szsoltzO2vr5KNt7d5/kmp+CxNad31qy6hK0L12MTF+GWF35d+UlDg9A6pZB9S9+7apdz73CruQdq3B7FxXR5f\
zN11tksMJu/dQmdlont2gf6AYn2M2ueW8vqlw/eo2LhsPm529n+cE/ZmgQfVrriwwNWhT8TU77HqnlmSJtJ6zOTeWoW75mNL\
DE/Pnyjxjb5YVK1KyadRKqXaUnpoh14+cWOx9+DktZy7DEb0x1SKPZ4+0rNmo6eJp3VtOsIUou1+LOcWa5ieumVMzPZCmbz8\
iBXpKpULnzkN9yuQ2s293A+atNuaincvsykUf7O0hy9pd9OVCMF3Y32pV9lcyPIbuCqaT100O3mtcmJksq93MKtKK2qctcpo\
ylQrsvKGIJvhu2/1U5fHPox92PW0s7srLTqicr5jVNVkRKSyqahabs9F1TZBx3a99ZL7734q7lIaKT95auPezzYSog1kG8uh\
XNk7bzLbmErfXfOi1aymfrspZsUrAsmMrY9pYH8fncjjfoMMN4mHPE6GJ1LEY0kSCp1SndzHWR4c/JJkTwwSX9saxZL0bqWD\
kuUVKr2IRrFPI7GqXBLKU8VUvpSL+MV29MWnu9Ob54pJTSl9scu+YGvdZHi9Q0FmYcQjSwOqYmY3U84TK+TvVU+qZ+bHF5gl\
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\
6N1/Of4/DDmqyA==";

/// The object bytes. base64 (RFC 4648) by hand: std has no base64 and a
/// dependency for a fixture would defeat its purpose.
pub fn object() -> Vec<u8> {
    let mut bits = 0u32;
    let mut n = 0u32;
    let mut z = Vec::with_capacity(OBJECT_B64.len() * 3 / 4);
    for c in OBJECT_B64.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'\n' | b'\r' | b'=' => continue,
            _ => panic!("bad base64 byte"),
        } as u32;
        bits = (bits << 6) | v;
        n += 6;
        if n >= 8 {
            n -= 8;
            z.push((bits >> n) as u8);
        }
    }
    miniz_oxide::inflate::decompress_to_vec_zlib(&z).expect("fixture is zlib")
}
