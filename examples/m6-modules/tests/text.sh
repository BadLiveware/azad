. "$(dirname "$0")/../lib/check.sh"
equal "AZAD" "$(echo azad | tr a-z A-Z)" "upper case"
equal 4 "$(printf azad | wc -c | tr -d ' ')" "length"
