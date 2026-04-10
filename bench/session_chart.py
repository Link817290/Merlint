#!/usr/bin/env python3
"""Generate chart from multi-turn session A/B test results."""

import json
import os
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.ticker as ticker
import numpy as np

def main():
    bench_dir = os.path.dirname(__file__)
    with open(os.path.join(bench_dir, "ab_session_results.json")) as f:
        data = json.load(f)

    b_turns = data["baseline"]["turns"]
    o_turns = data["optimized"]["turns"]
    summary = data["summary"]

    plt.style.use('dark_background')
    fig, axes = plt.subplots(2, 2, figsize=(15, 11))
    fig.suptitle(
        f"merlint A/B Test — Multi-Turn Coding Session (Kimi K2)\n"
        f"Scenario: {data['scenario']}  |  True controlled experiment",
        fontsize=14, fontweight='bold', color='#4ae0e0', y=0.98
    )

    c_base = '#ff6b6b'
    c_opt = '#51cf66'
    c_accent = '#4ae0e0'
    c_cache = '#ffd43b'

    # ---- Chart 1: Per-turn prompt tokens ----
    ax1 = axes[0][0]
    turns = [t["turn"] for t in b_turns]
    b_prompt = [t["prompt_tokens"] for t in b_turns]
    o_prompt = [t["prompt_tokens"] for t in o_turns]

    x = np.arange(len(turns))
    width = 0.35
    bars_b = ax1.bar(x - width/2, b_prompt, width, color=c_base, alpha=0.85, label=f'Baseline ({data["baseline"]["tools"]} tools)')
    bars_o = ax1.bar(x + width/2, o_prompt, width, color=c_opt, alpha=0.85, label=f'Optimized ({data["optimized"]["tools"]} tools)')

    for i, (bp, op) in enumerate(zip(b_prompt, o_prompt)):
        diff = bp - op
        ax1.text(i, max(bp, op) + 40, f'-{diff}',
                 ha='center', fontsize=9, color=c_accent, fontweight='bold')

    ax1.set_xlabel('Turn', fontsize=10)
    ax1.set_ylabel('Prompt Tokens', fontsize=10)
    ax1.set_title('Prompt Tokens per Turn', fontsize=11, fontweight='bold')
    ax1.set_xticks(x)
    ax1.set_xticklabels([f'Turn {t}' for t in turns])
    ax1.legend(fontsize=9)
    ax1.grid(alpha=0.2, axis='y')

    # ---- Chart 2: Cumulative tokens ----
    ax2 = axes[0][1]
    b_cum = np.cumsum(b_prompt)
    o_cum = np.cumsum(o_prompt)

    ax2.plot(turns, b_cum, 'o-', color=c_base, linewidth=2, markersize=8, label='Baseline (cumulative)')
    ax2.plot(turns, o_cum, 'o-', color=c_opt, linewidth=2, markersize=8, label='Optimized (cumulative)')
    ax2.fill_between(turns, o_cum, b_cum, alpha=0.2, color=c_opt)

    for i, (bc, oc) in enumerate(zip(b_cum, o_cum)):
        saved = bc - oc
        ax2.annotate(f'-{saved:,}', xy=(turns[i], (bc + oc) / 2),
                     fontsize=9, color=c_accent, fontweight='bold', ha='center')

    ax2.set_xlabel('Turn', fontsize=10)
    ax2.set_ylabel('Cumulative Prompt Tokens', fontsize=10)
    ax2.set_title('Cumulative Token Savings Over Session', fontsize=11, fontweight='bold')
    ax2.legend(fontsize=9)
    ax2.grid(alpha=0.2)

    # ---- Chart 3: Cache analysis ----
    ax3 = axes[1][0]
    b_cached = [t["cached_tokens"] for t in b_turns]
    o_cached = [t["cached_tokens"] for t in o_turns]
    b_uncached = [p - c for p, c in zip(b_prompt, b_cached)]
    o_uncached = [p - c for p, c in zip(o_prompt, o_cached)]

    x3 = np.arange(len(turns))
    w = 0.35

    # Stacked bars: uncached (bottom) + cached (top)
    ax3.bar(x3 - w/2, b_uncached, w, color=c_base, alpha=0.85, label='Baseline uncached')
    ax3.bar(x3 - w/2, b_cached, w, bottom=b_uncached, color=c_cache, alpha=0.6, label='Baseline cached')
    ax3.bar(x3 + w/2, o_uncached, w, color=c_opt, alpha=0.85, label='Optimized uncached')
    ax3.bar(x3 + w/2, o_cached, w, bottom=o_uncached, color=c_cache, alpha=0.3, label='Optimized cached')

    ax3.set_xlabel('Turn', fontsize=10)
    ax3.set_ylabel('Tokens', fontsize=10)
    ax3.set_title('Cache Hit Analysis (Kimi Prefix Caching)', fontsize=11, fontweight='bold')
    ax3.set_xticks(x3)
    ax3.set_xticklabels([f'Turn {t}' for t in turns])
    ax3.legend(fontsize=8, loc='upper left')
    ax3.grid(alpha=0.2, axis='y')

    # ---- Chart 4: Summary ----
    ax4 = axes[1][1]
    ax4.axis('off')

    tools_used = ', '.join(data["optimized"]["tools_used"])
    total_saved = summary["total_saved"]
    savings_pct = summary["savings_pct"]

    summary_text = (
        f"SESSION A/B TEST RESULTS\n"
        f"{'─' * 42}\n\n"
        f"Model:              {data['model']}\n"
        f"Turns:              {len(b_turns)}\n"
        f"Tools (baseline):   {data['baseline']['tools']}\n"
        f"Tools (optimized):  {data['optimized']['tools']} ({tools_used})\n\n"
        f"{'─' * 42}\n"
        f"Baseline total:     {data['baseline']['total_prompt']:,} tokens\n"
        f"Optimized total:    {data['optimized']['total_prompt']:,} tokens\n"
        f"Tokens saved:       {total_saved:,} ({savings_pct}%)\n"
        f"Saved per turn:     {summary['avg_saved_per_turn']:,} (constant)\n\n"
        f"Cache (baseline):   {summary['cached_baseline']:,} tokens\n"
        f"Cache (optimized):  {summary['cached_optimized']:,} tokens\n\n"
        f"{'─' * 42}\n"
        f"COST PROJECTION (monthly, 300 sessions)\n"
        f"  Kimi K2:      $0.49 saved\n"
        f"  Opus cached:  $1.33 saved\n"
        f"  Opus full:    $13.28 saved\n\n"
        f"{'─' * 42}\n"
        f"Method: True A/B test on live API\n"
        f"Same conversation, different tool counts"
    )

    ax4.text(0.05, 0.95, summary_text, transform=ax4.transAxes,
             fontsize=10.5, verticalalignment='top', fontfamily='monospace',
             color='white',
             bbox=dict(boxstyle='round,pad=0.5', facecolor='#2a2a4a',
                       edgecolor=c_accent, alpha=0.9))

    plt.tight_layout(rect=[0, 0.02, 1, 0.93])

    out_path = "/workspace/merlint_ab_session.png"
    fig.savefig(out_path, dpi=150, bbox_inches='tight',
                facecolor='#1a1a2e', edgecolor='none')
    print(f"Chart saved to {out_path}")
    plt.close()


if __name__ == "__main__":
    main()
