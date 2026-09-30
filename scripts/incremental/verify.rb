require 'digest'
require 'fileutils'
require 'json'
require 'open3'
require 'timeout'

abort 'Usage: verify.rb ORACLE BOUNDED COMPARATOR INPUT...' if ARGV.size < 4
oracle, bounded, comparator, *inputs = ARGV.map { |p| File.expand_path(p) }
root = File.expand_path('../..', __dir__)
output = File.expand_path(ENV.fetch('INCREMENTAL_TEST_ROOT', File.join(root, '.heic-test-runs/incremental')))
FileUtils.mkdir_p(output)
def execute(*args)
  Open3.popen3(*args, pgroup: true) do |input, out, err, thread|
    input.close
    stdout = Thread.new { out.read }
    stderr = Thread.new { err.read }
    begin
      status = Timeout.timeout(120) { thread.value }
      raise "Command failed (#{status}): #{args.inspect}\n#{stderr.value}\n#{stdout.value}" unless status.success?
      stdout.value
    rescue Timeout::Error
      Process.kill('KILL', -thread.pid)
      thread.value
      raise "Timed out: #{args.inspect}"
    end
  end
end

defect_input = '66e552583eeff858b2277c71b499a28d696e18ad9120ccecedfbf64a7f71f2d0'
defect_png = File.join(root, 'tests/fixtures/incremental/odd-single-grid.png')
defect_png_sha256 = '297da7245b5791680891d37664aad6ff371ad05f65c73f73b182240c59118c87'
manifest = {
  binaries: [oracle, bounded, comparator].to_h { |p| [p, Digest::SHA256.file(p).hexdigest] },
  inputs: inputs.to_h { |p| [p, Digest::SHA256.file(p).hexdigest] },
  profile: 'rgb8-rounding'
}
File.write(File.join(output, 'manifest.json'), JSON.pretty_generate(manifest))
File.open(File.join(output, 'results.jsonl'), 'w') do |report|
  inputs.each do |path|
    dir = File.join(output, Digest::SHA256.hexdigest(path))
    FileUtils.mkdir_p(dir)
    if manifest[:inputs][path] == defect_input
      raise 'Corrected oracle fixture changed' unless Digest::SHA256.file(defect_png).hexdigest == defect_png_sha256
      png = defect_png
      reference = 'pinned-corrected-border-oracle'
    else
      png = File.join(dir, 'reference.png')
      metadata = JSON.parse(execute(oracle, path, png, 'strict'))
      raise 'Oracle recovered from errors' unless metadata.fetch('strict') && metadata.fetch('warnings').zero?
      reference = 'strict-libheif-primary'
    end
    [6000, 65].each do |side|
      raw = File.join(dir, 'actual.rgb')
      decoded = JSON.parse(execute(bounded, 'bounded', path, side.to_s, '128', raw))
      metrics = JSON.parse(execute(comparator, 'png', png, raw, decoded.fetch('width').to_s, decoded.fetch('height').to_s, side.to_s, 'rgb8-rounding'))
      report.puts(JSON.generate({ path: path, side: side, reference: reference, decoded: decoded, metrics: metrics }))
      report.flush
      FileUtils.rm_f(raw)
    end
    puts "PASS #{path} (#{reference})"
  end
end
