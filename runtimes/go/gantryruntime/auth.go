package gantryruntime

import "context"

// DeveloperToken is the simplest auth flow: a fixed access token from the
// Box developer console. The other three flows (CCG, JWT, OAuth 2.0
// authorization code) implement the same TokenSource interface and can be
// passed to New in its place.
func DeveloperToken(token string) TokenSource { return developerToken(token) }

type developerToken string

func (t developerToken) AccessToken(context.Context) (string, error) { return string(t), nil }
