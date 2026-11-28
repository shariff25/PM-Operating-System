//! Two RSA keys generated for tests only (`openssl genrsa 2048`), never used anywhere else: A
//! is the mock identity provider's published key, B one it does not publish (or publishes
//! later). PKCS#1 DER in base64, and each modulus in base64url for the JWKS.

pub const KEY_A_DER: &str = concat!(
    "MIIEpQIBAAKCAQEA4MKvZf7hQ9BNP/COfHIx6H5LR52nRzOgte3lPKtxRCpP1tDmpAAiXTAlN73IevPIzSNqAOirJfRQ",
    "66tDFaBRUagk4ZA7mnPnQgrPItXB4+uG9ccugQkEDy56i3+vlrFvSm6MiDOghFDCLEFdkeYwlIdm8l1Ysiz75ZjVc4YB",
    "2F25i3oYXMzWoo2IqBmyRGvSmzYZUlKiTzqmfswEuSi8JEt+V8HtptA3fFTxek1JVHTcibEvZ+mnb7nLCtIV8UTFLvs7",
    "qmBWrJeiRJSEi4skiYI1y585ec+Q4lvkXwtJm7w3AGqp/RjrlG427LntPAwVVQOVWest7zt+NX1GivwAPQIDAQABAoIB",
    "ADLGYHPxAsg8Vjs/15yO+q3cXBrLZ4cLB38aBVuxmlatHVi43CXH/vtg3UOMrO3zxVTsb23RohpiqWY4DCFmVICXNO+9",
    "Eu33SZLOmNE0i4b3BgffjaaqV/+q/xWlDJCFOJBOzZIWYaiKivcO9Dm50Bc5yHyvsFIeHfDWGEBvEVskkt0RlLgjTLyf",
    "XbgTUo7CXFZ7mk8uq8j9EVb6apAJ0/wOOQvCcCRTYvi08tKjfy2bwPqJtlVIM+gpKAgGsRhk47biA4mDu0rW3PfU87Q2",
    "mYgXnZjeRvDt7z/A3o7L2COIs/VcxnMs8kfLtYyen/bXOJl673NMCtfmAPgLB1Bmt7ECgYEA8K9Jab8vvuAZ8lynYCsc",
    "l1+JBYsVm6JG0q50CMfOJ8ae1WCdySwQs1A1EAvEDBweLOHoxgum3d+CKHfhUxmb0zeLgdUMqYbAkRh4jiusOZBrmcvw",
    "pu+DPI7PZHIGSJLygTDj0TVOwOfOPRFmWb6tB9/XD44ErE04e6pbor799tECgYEA7w/+1dh1n6KHviA8N/qpje5JQ2bg",
    "BHj4F7cnmTPPWuNFYwTEU0GUJjTMJK8rVGWln4WjXhLdFPAZnUD44JB/mIcR90kd53g9C4gx1//s7ZkSf9uQF2XLWf2D",
    "vIHEusYXCm2bb2eUQyMaqNQ4mtN7f2CHFP0+xWFy/n8dNQWwJa0CgYEAw92R+7l3jHjpBJ+8IfnTkahQZK90DSx5QDhx",
    "+zdGoH31giXXIgB3MsWB+SXqDATR1aqeeJTA3exnCrHML4k2kyOvpx+O52Le0PU+HkySImJR/l3F4MQnY5/ATLiyPls/",
    "eD0mGHmuNjQuVOoVBdGzFCZn6KOlT+ajoZfYbM/K8xECgYEA2PI5HzOKus2yRhXW0C5HxKekEOjBDw4AhvUNmHhuClsJ",
    "Uuy2hZ+TpIBxajEVL0f3X6bVNMAFpcSmWhDmSlSXloGtCi5uVurE6zwzj4OPLaemJCXKw5D7U7U+4ckaES1HJ42Eazsi",
    "TzMCNIgyvj6Banzc2W1gk5r10oLkrIN28RkCgYEA7l4diOTypj2DmQwauJTBzgz1oAQn5dZ+UvM0YD+vTbISfalayMtI",
    "M/hTNyU+rdW1nmELi2jKGi1y0dIuHCGPheYukL/DIegBG022YeGZuK3MS2bm3ojAUM+gm2gLtCoxMfDL08fvGQ6PheKV",
    "+Su/WfsEHi5RiXa3+6bd6TAtZH0=",
);
pub const KEY_A_N: &str = concat!(
    "4MKvZf7hQ9BNP_COfHIx6H5LR52nRzOgte3lPKtxRCpP1tDmpAAiXTAlN73IevPIzSNqAOirJfRQ66tDFaBRUagk4ZA7",
    "mnPnQgrPItXB4-uG9ccugQkEDy56i3-vlrFvSm6MiDOghFDCLEFdkeYwlIdm8l1Ysiz75ZjVc4YB2F25i3oYXMzWoo2I",
    "qBmyRGvSmzYZUlKiTzqmfswEuSi8JEt-V8HtptA3fFTxek1JVHTcibEvZ-mnb7nLCtIV8UTFLvs7qmBWrJeiRJSEi4sk",
    "iYI1y585ec-Q4lvkXwtJm7w3AGqp_RjrlG427LntPAwVVQOVWest7zt-NX1GivwAPQ",
);
pub const KEY_B_DER: &str = concat!(
    "MIIEowIBAAKCAQEA8TdjSqvK87MYEhTaAtpIGoaV1PVS80PftrXY14LtCecIvBw9Vprhx097Nwte+Jls+ap1hxEUZZkQ",
    "evoWSA3nXZsC5LWZ7TS5lSW4H3R+gHHoz6xJq9TY+NAFKVAdv+VydilWCHSKxdlfBYMUzHSnJHAwpTBM8keDRZmfPYFr",
    "Ar8srZPru10X6BHe+uvZSDUHB9ixvCZ06FqGk93NLkUZSfqmK88C7Rzq7+77iOVHmBDARmsqkeBGdRzCIg6fnwUM2X5l",
    "3h2Io2bixKgBMM8LtteQrkrzFTpqNIrt+tip9LDdFFlU+wn2Ka056e0wVC0+MH/YEuYzY5x8YLzg/HuzIQIDAQABAoIB",
    "ADXNYqB6Yn2KZhVMYNcvjFMcT0mtgXfAnSLR+0CQk6H4jfZ1Pwo06CSuWf+MUf3VrNygNHAws1G4+xfO9c7TCRbUFyhf",
    "yCpELxuxCtk7QOcR4TmHBnSk+FNBBNGy7LvLaAUqkaW3pudb5Wmjk+1JnKlQO81wkU0d/c31prLPjciR4D4TLcGROvCr",
    "kULm0TVaPoESyXcTRS5qQy5bcvbjI1YV02v+vVm0iHzEAei3+wLwm5wVx8FHVESd9osPQbj5OU7qpsaL4/yyHFOmrseD",
    "f5WcpBjxcIg2RaLUHJbaIcWeBPAQu87a/+j0PpVwaa3x+ZN+ui47EDifQ5xRhUOrMDUCgYEA++qWC0mcoYU8N83QgBU5",
    "S7wF5EK48TxjqnLqzx46nKmR9YPQY6ia1QtQ1GHpuE+en4jW2VgiGEBjj8Qsb1FxiyEW48rDSDmk4LasFGgB2Kxpn81P",
    "n651POvPN0OJJdYs2KelXoF2dFHGiC00RjMSqcJACKNFbFZrnSpoLx/Zws0CgYEA9SBl/6OtVpI3O2bqbI+mSvVJbiq3",
    "UFaIX5MQ0fNM57khSQWmrL6q3G6jMZBr3KOcnio6L27rZMyVPWZFeTN4Piy+XDjlvMIJ9+a6h0V/aNRxG2uzgo1rnSnz",
    "LKCFuspQC++qjmF217SwS0QDq8FJqbs3vzPIMPiUEwk3FVLHuaUCgYAeNYRmuB4bVHHjM0Aki+cCCPsf40xwSVCeWQeF",
    "bNeUf9Ur/vGXcdmXYLMBtPglDQ81jGBPUf1MsTP/QoQdmUOnboFjMaZ3W8pE1EMQc2R9SjekXSeGO5J9ir2fn+6rZeKc",
    "3IW9LoM48bGAFQOmw2sihlPNdV7W3gorn7E8rXIcpQKBgANKnBQCrqEq9XLmvt8WTrVS9dl02WFKyCoAX4lyKuYxpjDE",
    "0qgfJ0wucCKKPeiIieHVz1IzuxVvadGZPfWSq1qqXyP935fZQykBACAw3rUZpSspFw/5tig2F/3RafpIHSsL3M5soJvk",
    "4TkhgKsdvU96JBV9LIL704Et7n9qzSPBAoGBAIeQihazQuL/uuTkCFjriNMXoTzHSuzfXBHB7JOKPeobxDp90njCYAiv",
    "5/Q2SDotmHMiF56nUcuwE5HWA4zmXr8vpGIMEqJOREO1eICK56sWlE1wS6Ews0x6vQVSaEuooYyJR9VWo5aaxR4ir9HK",
    "vfk83PvoRJpFTjFwyrHIO/bw",
);
pub const KEY_B_N: &str = concat!(
    "8TdjSqvK87MYEhTaAtpIGoaV1PVS80PftrXY14LtCecIvBw9Vprhx097Nwte-Jls-ap1hxEUZZkQevoWSA3nXZsC5LWZ",
    "7TS5lSW4H3R-gHHoz6xJq9TY-NAFKVAdv-VydilWCHSKxdlfBYMUzHSnJHAwpTBM8keDRZmfPYFrAr8srZPru10X6BHe",
    "-uvZSDUHB9ixvCZ06FqGk93NLkUZSfqmK88C7Rzq7-77iOVHmBDARmsqkeBGdRzCIg6fnwUM2X5l3h2Io2bixKgBMM8L",
    "tteQrkrzFTpqNIrt-tip9LDdFFlU-wn2Ka056e0wVC0-MH_YEuYzY5x8YLzg_HuzIQ",
);
