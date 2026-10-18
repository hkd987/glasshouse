# At login, one line on your agents: "◉ 1 needs you   ● 2 working".
# /etc/profile sources this for sh login shells; quiet for scripts and when
# glassd isn't running.
case $- in
*i*) ;;
*) return 0 ;;
esac
if [ -S /var/run/glasshouse/control.sock ] && [ -x /usr/local/bin/glassctl ]; then
	# Captured output isn't a terminal; keep the colors if ours is.
	if [ -t 1 ]; then
		_glass_summary=$(CLICOLOR_FORCE=1 /usr/local/bin/glassctl tasks --summary 2>/dev/null)
	else
		_glass_summary=$(/usr/local/bin/glassctl tasks --summary 2>/dev/null)
	fi
	[ -n "$_glass_summary" ] && printf '%s\n\n' "$_glass_summary"
	unset _glass_summary
fi
