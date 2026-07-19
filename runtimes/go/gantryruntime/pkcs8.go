// Decryption of an encrypted PKCS#8 private key (RFC 5958 EncryptedPrivateKeyInfo
// with an RFC 8018 PBES2 scheme) — the format Box's box_config.json ships, a
// `-----BEGIN ENCRYPTED PRIVATE KEY-----` block. Go's standard library parses
// *unencrypted* PKCS#8 (x509.ParsePKCS8PrivateKey) and can decrypt only the
// legacy RFC 1423 PEM encryption (x509.DecryptPEMBlock), not PBES2, and its
// crypto/pbkdf2 arrived only in Go 1.24 (this module's floor is 1.23). So the
// small PBES2 (PBKDF2 + AES-CBC) path lives here — pure standard library, no
// third-party dependency, matching how the Rust/TS/Java runtimes handle the
// same key with their platforms' crypto.
package gantryruntime

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/hmac"
	"crypto/sha1"
	"crypto/sha256"
	"crypto/sha512"
	"encoding/asn1"
	"errors"
	"fmt"
	"hash"
)

var (
	oidPBES2  = asn1.ObjectIdentifier{1, 2, 840, 113549, 1, 5, 13}
	oidPBKDF2 = asn1.ObjectIdentifier{1, 2, 840, 113549, 1, 5, 12}

	oidHMACSHA1   = asn1.ObjectIdentifier{1, 2, 840, 113549, 2, 7}
	oidHMACSHA224 = asn1.ObjectIdentifier{1, 2, 840, 113549, 2, 8}
	oidHMACSHA256 = asn1.ObjectIdentifier{1, 2, 840, 113549, 2, 9}
	oidHMACSHA384 = asn1.ObjectIdentifier{1, 2, 840, 113549, 2, 10}
	oidHMACSHA512 = asn1.ObjectIdentifier{1, 2, 840, 113549, 2, 11}

	oidAES128CBC = asn1.ObjectIdentifier{2, 16, 840, 1, 101, 3, 4, 1, 2}
	oidAES192CBC = asn1.ObjectIdentifier{2, 16, 840, 1, 101, 3, 4, 1, 22}
	oidAES256CBC = asn1.ObjectIdentifier{2, 16, 840, 1, 101, 3, 4, 1, 42}
)

// algorithmIdentifier is the ASN.1 AlgorithmIdentifier (an OID plus opaque
// parameters interpreted per algorithm).
type algorithmIdentifier struct {
	Algorithm  asn1.ObjectIdentifier
	Parameters asn1.RawValue `asn1:"optional"`
}

// encryptedPrivateKeyInfo is RFC 5958 EncryptedPrivateKeyInfo.
type encryptedPrivateKeyInfo struct {
	Algo          algorithmIdentifier
	EncryptedData []byte
}

// pbes2Params is RFC 8018 PBES2-params: a key-derivation function and an
// encryption scheme.
type pbes2Params struct {
	KeyDerivationFunc algorithmIdentifier
	EncryptionScheme  algorithmIdentifier
}

// pbkdf2Params is RFC 8018 PBKDF2-params. keyLength and prf are optional; an
// absent prf means the default, HMAC-SHA1. (Salt as a CHOICE otherSource is not
// used by Box, so it is modeled as the specified OCTET STRING form.)
type pbkdf2Params struct {
	Salt       []byte
	Iterations int
	KeyLength  int                 `asn1:"optional"`
	PRF        algorithmIdentifier `asn1:"optional"`
}

// decryptPKCS8 decrypts a PBES2-encrypted PKCS#8 key (PBKDF2 + AES-CBC) with the
// given password and returns the inner unencrypted PKCS#8 DER, ready for
// x509.ParsePKCS8PrivateKey. It supports the schemes real Box keys use —
// PBKDF2 with an HMAC-SHA1/224/256/384/512 PRF and AES-128/192/256-CBC — and
// reports a clear error for anything else.
func decryptPKCS8(der, password []byte) ([]byte, error) {
	var enc encryptedPrivateKeyInfo
	if _, err := asn1.Unmarshal(der, &enc); err != nil {
		return nil, fmt.Errorf("parsing EncryptedPrivateKeyInfo: %w", err)
	}
	if !enc.Algo.Algorithm.Equal(oidPBES2) {
		return nil, fmt.Errorf("unsupported key encryption scheme %v (only PBES2 is supported)", enc.Algo.Algorithm)
	}

	var params pbes2Params
	if _, err := asn1.Unmarshal(enc.Algo.Parameters.FullBytes, &params); err != nil {
		return nil, fmt.Errorf("parsing PBES2 parameters: %w", err)
	}
	if !params.KeyDerivationFunc.Algorithm.Equal(oidPBKDF2) {
		return nil, fmt.Errorf("unsupported key derivation function %v (only PBKDF2 is supported)", params.KeyDerivationFunc.Algorithm)
	}

	var kdf pbkdf2Params
	if _, err := asn1.Unmarshal(params.KeyDerivationFunc.Parameters.FullBytes, &kdf); err != nil {
		return nil, fmt.Errorf("parsing PBKDF2 parameters: %w", err)
	}
	if kdf.Iterations <= 0 {
		return nil, fmt.Errorf("invalid PBKDF2 iteration count %d", kdf.Iterations)
	}
	prf, err := prfHash(kdf.PRF)
	if err != nil {
		return nil, err
	}

	keyLen, iv, err := aesCBCParams(params.EncryptionScheme)
	if err != nil {
		return nil, err
	}
	// The AES scheme's OID is authoritative for the key size. A PBKDF2 keyLength,
	// when present, must agree with it; a mismatched value (or a negative/huge one
	// from a malformed key) is rejected rather than honored — trusting it would
	// derive the wrong AES variant or panic the dk[:keyLen] slice.
	if kdf.KeyLength != 0 && kdf.KeyLength != keyLen {
		return nil, fmt.Errorf("PBKDF2 key length %d does not match the %d-byte AES-CBC key", kdf.KeyLength, keyLen)
	}

	block, err := aes.NewCipher(pbkdf2Key(password, kdf.Salt, kdf.Iterations, keyLen, prf))
	if err != nil {
		return nil, fmt.Errorf("building AES cipher: %w", err)
	}
	if len(enc.EncryptedData) == 0 || len(enc.EncryptedData)%block.BlockSize() != 0 {
		return nil, errors.New("encrypted key is not a whole number of cipher blocks")
	}
	out := make([]byte, len(enc.EncryptedData))
	cipher.NewCBCDecrypter(block, iv).CryptBlocks(out, enc.EncryptedData)
	return pkcs7Unpad(out, block.BlockSize())
}

// prfHash maps a PBKDF2 PRF AlgorithmIdentifier to its hash constructor. An
// absent PRF (the zero value) is the RFC 8018 default, HMAC-SHA1.
func prfHash(id algorithmIdentifier) (func() hash.Hash, error) {
	switch {
	case len(id.Algorithm) == 0, id.Algorithm.Equal(oidHMACSHA1):
		return sha1.New, nil
	case id.Algorithm.Equal(oidHMACSHA224):
		return sha256.New224, nil
	case id.Algorithm.Equal(oidHMACSHA256):
		return sha256.New, nil
	case id.Algorithm.Equal(oidHMACSHA384):
		return sha512.New384, nil
	case id.Algorithm.Equal(oidHMACSHA512):
		return sha512.New, nil
	default:
		return nil, fmt.Errorf("unsupported PBKDF2 PRF %v", id.Algorithm)
	}
}

// aesCBCParams returns the AES key length and CBC IV for an AES-CBC encryption
// scheme (its parameters are the IV as an OCTET STRING).
func aesCBCParams(scheme algorithmIdentifier) (keyLen int, iv []byte, err error) {
	switch {
	case scheme.Algorithm.Equal(oidAES128CBC):
		keyLen = 16
	case scheme.Algorithm.Equal(oidAES192CBC):
		keyLen = 24
	case scheme.Algorithm.Equal(oidAES256CBC):
		keyLen = 32
	default:
		return 0, nil, fmt.Errorf("unsupported encryption cipher %v (only AES-CBC is supported)", scheme.Algorithm)
	}
	if _, err := asn1.Unmarshal(scheme.Parameters.FullBytes, &iv); err != nil {
		return 0, nil, fmt.Errorf("parsing cipher IV: %w", err)
	}
	if len(iv) != aes.BlockSize {
		return 0, nil, fmt.Errorf("cipher IV is %d bytes, want %d", len(iv), aes.BlockSize)
	}
	return keyLen, iv, nil
}

// pbkdf2Key derives a keyLen-byte key from the password and salt (RFC 8018
// PBKDF2). Hand-rolled because crypto/pbkdf2 is Go 1.24+ and this module targets
// 1.23; the runtime stays dependency-free.
func pbkdf2Key(password, salt []byte, iterations, keyLen int, h func() hash.Hash) []byte {
	prf := hmac.New(h, password)
	hashLen := prf.Size()
	blocks := (keyLen + hashLen - 1) / hashLen
	dk := make([]byte, 0, blocks*hashLen)
	var counter [4]byte
	for i := 1; i <= blocks; i++ {
		counter[0], counter[1], counter[2], counter[3] = byte(i>>24), byte(i>>16), byte(i>>8), byte(i)
		prf.Reset()
		prf.Write(salt)
		prf.Write(counter[:])
		u := prf.Sum(nil)
		t := make([]byte, hashLen)
		copy(t, u)
		for n := 2; n <= iterations; n++ {
			prf.Reset()
			prf.Write(u)
			u = prf.Sum(u[:0])
			for x := range t {
				t[x] ^= u[x]
			}
		}
		dk = append(dk, t...)
	}
	return dk[:keyLen]
}

// pkcs7Unpad strips and validates PKCS#7 padding. A wrong passphrase yields
// garbage plaintext that almost always fails here (or in the subsequent PKCS#8
// parse), so this doubles as the wrong-passphrase signal.
func pkcs7Unpad(data []byte, blockSize int) ([]byte, error) {
	if len(data) == 0 || len(data)%blockSize != 0 {
		return nil, errors.New("decrypted key has an invalid length (wrong passphrase?)")
	}
	pad := int(data[len(data)-1])
	if pad == 0 || pad > blockSize || pad > len(data) {
		return nil, errors.New("decrypted key has invalid padding (wrong passphrase?)")
	}
	for _, b := range data[len(data)-pad:] {
		if int(b) != pad {
			return nil, errors.New("decrypted key has invalid padding (wrong passphrase?)")
		}
	}
	return data[:len(data)-pad], nil
}
