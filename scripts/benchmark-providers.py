#!/usr/bin/env python3
"""Verified whole-image conversion comparisons; Python standard library only."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import math
import random
import shutil
import statistics
import subprocess
import tempfile
import time


TIMEOUT = 300.0
COMMAND_LOG = []
OWNED_REPORT = None


def run(command):
    started = time.perf_counter()
    record = {"command": command}
    COMMAND_LOG.append(record)
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=TIMEOUT)
    except subprocess.TimeoutExpired as error:
        record.update(status="timeout", seconds=time.perf_counter() - started,
                      stdout=str(error.stdout or ""), stderr=str(error.stderr or ""))
        raise RuntimeError(f"command timed out after {TIMEOUT}s: {command!r}") from error
    record.update(status="ok" if result.returncode == 0 else "failed",
                  returncode=result.returncode, seconds=time.perf_counter() - started,
                  stdout=result.stdout, stderr=result.stderr)
    if result.returncode:
        raise RuntimeError(f'{command!r} exited {result.returncode}:\n{result.stdout}\n{result.stderr}')
    return result.stdout.strip()


def digest(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def ps_literal(value):
    return "'" + str(value).replace("'", "''") + "'"


def verify_output(destination, verification, image_format, size, expected):
    run(['qemu-img', 'convert', '-f', image_format, '-O', 'raw', str(destination), str(verification)])
    if verification.stat().st_size != size or digest(verification) != expected:
        raise RuntimeError('logical byte verification failed')


def publish_no_overwrite(staged, destination):
    """Publish the verified inode without replacing an existing path."""
    with staged.open('rb') as output:
        os.fsync(output.fileno())
    os.link(staged, destination)
    staged.unlink()
    if os.name == 'posix':
        directory = os.open(destination.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)


def summarize(rows, providers, seed):
    """Paired bootstrap resamples rounds, never individual providers independently."""
    summary = {}
    for provider in providers:
        selected = [r for r in rows if r['provider'] == provider and r['status'] == 'ok' and not r['warmup']]
        values = [r['mib_per_second'] for r in selected]
        summary[provider] = {'successful_samples': len(values),
                             'failed_samples': sum(r['status'] != 'ok' for r in rows if r['provider'] == provider),
                             'median_mib_per_second': statistics.median(values) if values else None,
                             'min_mib_per_second': min(values) if values else None,
                             'max_mib_per_second': max(values) if values else None,
                             'median_seconds': statistics.median(r['seconds'] for r in selected) if values else None}
    ratios = {}
    baseline = {r['sample']: r['mib_per_second'] for r in rows
                if r['provider'] == 'virtdisk' and r['status'] == 'ok' and not r['warmup']}
    for provider in providers:
        if provider == 'virtdisk':
            continue
        paired = [(baseline[r['sample']], r['mib_per_second']) for r in rows
                  if r['provider'] == provider and r['status'] == 'ok' and not r['warmup'] and r['sample'] in baseline]
        if not paired:
            continue
        def ratio(pairs):
            return statistics.median(a for a, _ in pairs) / statistics.median(b for _, b in pairs)
        rng = random.Random(seed)
        bootstrap = sorted(ratio(rng.choices(paired, k=len(paired))) for _ in range(2000))
        ratios[provider] = {'virtdisk_over_provider': ratio(paired), 'paired_rounds': len(paired),
                            'bootstrap_95_percent_interval': [bootstrap[49], bootstrap[1949]] if len(paired) >= 10 else 'unavailable',
                            'method': 'percentile paired-round bootstrap of ratio of median throughputs; 2000 resamples'}
    return summary, ratios


def checkpoint(path, report):
    temporary = path.with_name(path.name + '.checkpoint')
    with temporary.open('w') as output:
        json.dump(report, output, indent=2)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--virtdisk', required=True, help='release CLI executable')
    parser.add_argument('--providers', nargs='+', choices=['virtdisk', 'qemu', 'virtualbox', 'hyperv'], default=['virtdisk', 'qemu'])
    parser.add_argument('--input-format', choices=['raw', 'vhdx'], default='raw')
    parser.add_argument('--format', choices=['raw', 'qcow2', 'vdi', 'vmdk', 'vhdx'], default='qcow2')
    parser.add_argument('--size-mib', type=int, default=256)
    parser.add_argument('--samples', type=int, default=10)
    parser.add_argument('--pattern', choices=['dense', 'sparse'], default='dense')
    parser.add_argument('--work-dir', type=Path, help='filesystem to benchmark')
    parser.add_argument('--output', type=Path, required=True, help='new JSON report (never overwritten)')
    parser.add_argument('--allocation-policy', choices=['default', 'sparse', 'dense'], default='default')
    parser.add_argument('--tier', choices=['default', 'matched'], default='default',
                        help='matched includes wrapper verification and no-overwrite publication in timing')
    parser.add_argument('--timeout', type=float, default=300, help='per-command timeout in seconds')
    parser.add_argument('--seed', type=int, default=42)
    parser.add_argument('--session-id', default=None, help='independent session identity')
    parser.add_argument('--environment-json', type=Path, help='operator manifest: filesystem, VM storage chain, RAM, CPU, host load and release flags')
    args = parser.parse_args()
    global TIMEOUT, OWNED_REPORT
    TIMEOUT = args.timeout
    if not math.isfinite(TIMEOUT) or TIMEOUT <= 0:
        parser.error('timeout must be finite and positive')
    if args.size_mib <= 0 or args.samples <= 0:
        parser.error('size and samples must be positive')
    if len(set(args.providers)) != len(args.providers):
        parser.error('providers must be unique')
    if args.output.exists():
        parser.error('output already exists')
    if args.allocation_policy == 'dense':
        if any(p in args.providers for p in ('virtualbox', 'hyperv')):
            parser.error('dense policy cannot be equalized for VirtualBox/Hyper-V')
        if 'virtdisk' in args.providers and args.format != 'qcow2':
            parser.error('virtdisk dense conversion policy is currently only explicit for qcow2')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('x') as output:
        OWNED_REPORT = args.output
        json.dump({'status': 'initializing'}, output)
    binary = str(Path(args.virtdisk).resolve())
    # qemu-img is also the independent byte-verification backend for every provider.
    versions = {'qemu': run(['qemu-img', '--version'])}
    if 'virtualbox' in args.providers:
        versions['virtualbox'] = run(['VBoxManage', '--version'])
        if args.input_format != 'raw' or args.format not in ('vdi', 'vmdk'):
            parser.error('VirtualBox comparison requires raw input and vdi/vmdk output')
    if 'hyperv' in args.providers:
        if args.input_format != 'vhdx' or args.format != 'vhdx':
            parser.error('Hyper-V comparison requires vhdx input and vhdx output')
        versions['hyperv'] = run(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', 'Import-Module Hyper-V; (Get-Module Hyper-V).Version.ToString()'])
    report = {'schema_version': 2, 'status': 'running', 'comparison_tier': 'default_command_latency' if args.tier == 'default' else 'matched_application_workflow',
              'durability_equivalent': False,
              'matched_applicability': 'workflow wrapper only; allocation policy must be explicit and block/layout differences reviewed' if args.allocation_policy == 'default' else 'explicit zero policy; block/layout differences still require review',
              'tier_exclusions': {'matched_application_workflow': 'selected' if args.tier == 'matched' else 'select --tier matched for timed wrapper work',
                                  'library_and_phase_costs': 'requires separate in-process instrumentation; Hyper-V includes startup'},
              'profile': {'input_format': args.input_format, 'output_format': args.format,
                          'sparsity': args.allocation_policy, 'block_size': 'provider defaults; unequal',
                          'publication': ('fsynced file and directory, hard-link no-overwrite' if os.name == 'posix' else 'fsynced file, hard-link no-overwrite; directory durability not established') if args.tier == 'matched' else 'provider defaults',
                          'verification': 'QEMU decode, exact logical size and SHA-256 inside wrapper timing' if args.tier == 'matched' else 'independent verification outside timing',
                          'remaining_differences': 'virtdisk also performs built-in verification; metadata layouts, allocation and flush behavior differ'},
              'native_acceptance': 'unavailable; QEMU byte decoding does not establish native consumer acceptance',
              'session_id': args.session_id,
              'environment': json.loads(args.environment_json.read_text()) if args.environment_json else 'unavailable',
              'command_log': COMMAND_LOG,
              'platform': platform.platform(), 'cpu': platform.processor(),
              'versions': versions, 'arguments': {k: str(v) if isinstance(v, Path) else v for k, v in vars(args).items()},
              'virtdisk_binary_sha256': digest(Path(binary)),
              'executable_sha256': {name: digest(Path(resolved)) for name in [binary, 'qemu-img', 'VBoxManage', 'powershell.exe'] if (resolved := shutil.which(name))},
              'cache_policy': 'warm-up per provider; OS cache retained; randomized measured order',
              'timing': 'process start to exit; independent verification excluded' if args.tier == 'default' else 'tool conversion, full independent decode/hash, file sync and no-overwrite publication; setup excluded',
              'results': []}
    with tempfile.TemporaryDirectory(prefix='virtdisk-benchmark-', dir=args.work_dir) as temporary:
        root = Path(temporary).resolve()
        raw = root / 'seed.raw'
        rng = random.Random(args.seed)
        size = args.size_mib * 1048576
        with raw.open('xb') as output:
            for index in range(args.size_mib):
                if args.pattern == 'sparse' and index % 16:
                    output.seek(1048576, os.SEEK_CUR)
                else:
                    output.write(rng.randbytes(1048576))
            output.truncate(size)
            output.flush()
            os.fsync(output.fileno())
        expected = digest(raw)
        source = raw
        if args.input_format != 'raw':
            source = root / 'seed.vhdx'
            run([binary, 'convert', str(raw), 'raw', str(source), 'vhdx'])
        report['source_sha256'] = expected
        report['input_provenance'] = {'generator': 'Python Random.randbytes; one MiB blocks; sparse one block per sixteen', 'seed': args.seed, 'logical_bytes': size}
        checkpoint(args.output, report)
        for sample in range(args.samples + 1):
            order = list(args.providers)
            rng.shuffle(order)
            for provider in order:
                published = root / f'{provider}-{sample}.{args.format}'
                destination = published.with_name(published.name + '.staged') if args.tier == 'matched' else published
                if provider == 'virtdisk':
                    command = [binary, 'convert', str(source), args.input_format, str(destination), args.format]
                    if args.allocation_policy != 'default':
                        command.append('sparse' if args.allocation_policy == 'sparse' else 'default')
                elif provider == 'qemu':
                    command = ['qemu-img', 'convert', '-f', args.input_format, '-O', args.format]
                    if args.allocation_policy != 'default':
                        command.extend(['-S', '0' if args.allocation_policy == 'dense' else '4k'])
                    command.extend([str(source), str(destination)])
                elif provider == 'virtualbox':
                    command = ['VBoxManage', 'convertfromraw', str(source), str(destination), '--format', args.format.upper(), '--variant', 'Standard']
                else:
                    script = f"$ErrorActionPreference='Stop'; Convert-VHD -Path {ps_literal(source)} -DestinationPath {ps_literal(destination)} -VHDType Dynamic"
                    command = ['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script]
                row = {'provider': provider, 'sample': sample, 'warmup': sample == 0,
                       'command': command, 'status': 'running', 'verified': False,
                       'cpu_seconds': 'unavailable', 'peak_rss_bytes': 'unavailable',
                       'bytes_read': 'unavailable', 'bytes_written': 'unavailable'}
                report['results'].append(row)
                checkpoint(args.output, report)
                verification = root / f'verification-{provider}-{sample}.raw'
                try:
                    start = time.perf_counter()
                    run(command)
                    if args.tier == 'matched':
                        verify_output(destination, verification, args.format, size, expected)
                        publish_no_overwrite(destination, published)
                        destination = published
                    elapsed = time.perf_counter() - start
                    row.update(seconds=elapsed, mib_per_second=args.size_mib / elapsed)
                    if args.tier == 'default':
                        verify_output(destination, verification, args.format, size, expected)
                    metadata = destination.stat()
                    row.update(status='ok', verified=True, container_bytes=metadata.st_size,
                               allocated_bytes=metadata.st_blocks * 512 if hasattr(metadata, 'st_blocks') else 'unavailable')
                    verification.unlink()
                    destination.unlink()
                except Exception as error:
                    row.update(status='failed', error=str(error))
                    # TemporaryDirectory normally removes artifacts: copy failures beside the report.
                    diagnostics = args.output.with_name(args.output.name + '.diagnostics')
                    diagnostics.mkdir(exist_ok=True)
                    for artifact in (destination, verification):
                        if artifact.exists():
                            shutil.copy2(artifact, diagnostics / artifact.name)
                    row['diagnostics'] = str(diagnostics)
                checkpoint(args.output, report)
        report['summary'], report['throughput_ratios'] = summarize(report['results'], args.providers, args.seed)
        report['status'] = 'failed' if any(r['status'] != 'ok' for r in report['results']) else 'complete'
    checkpoint(args.output, report)
    print(json.dumps(report['summary'], indent=2))
    if report['status'] != 'complete':
        raise SystemExit(1)


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        # Preserve setup/manifest/verification failures as machine-readable evidence too.
        import sys
        if OWNED_REPORT is not None:
            report_path = OWNED_REPORT
            if report_path.exists():
                try:
                    failed = json.loads(report_path.read_text())
                    if failed.get('status') in ('initializing', 'running'):
                        failed.update(status='failed', error=str(error), command_log=COMMAND_LOG)
                        checkpoint(report_path, failed)
                except (OSError, ValueError):
                    pass
        raise
