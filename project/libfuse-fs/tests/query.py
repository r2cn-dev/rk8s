import re
import argparse

# Store request/response pairs
request_map = {}

def parse_log_line(line,filters=None):
    """
    Parse a single log line to extract relevant information.

    Args:
        line (str): A single line from the log file.

    Returns:
        dict or None: Parsed log information as a dictionary, or None if the line doesn't match.
    """
    # Try to parse request log format
    req_pattern = r'ID:([^|]+)\|(\[[\w]+\])\s+REQRequest\s+{([^}]+)}\s*-\s*-\s*Call:\s*(.+)'
    req_match = re.match(req_pattern, line)
    if req_match:
        req_id = req_match.group(1).strip()
        request_info = {
            'id': req_id,
            'type': 'request',
            'function': req_match.group(2).strip('[]'),
            'request': req_match.group(3).strip(),
            'params': req_match.group(4).strip()
        }
        show_line = True
        if filters.get('id') and filters['id'] not in request_info['id']:
            show_line = False
        if filters.get('functions') and request_info['type'] == 'request':
            if not any(func.lower() in request_info['function'].lower() for func in filters['functions']):
                show_line = False

        if show_line:
            request_map[req_id] = request_info
            return request_info
    
    # Try to parse response log format
    # Try to parse response log format
    resp_pattern = r'ID:([a-f0-9-]+)(?:\s*\[[\w]+\])?(.*)'
    resp_match = re.match(resp_pattern, line)
    if resp_match:
        resp_id = resp_match.group(1).strip()
        
        if resp_id in request_map:
            request_info = request_map.pop(resp_id)
            return {
                'id': resp_id,
                'type': 'response',
                'message': resp_match.group(2),
                'function': request_info.get('function'),
                'request': request_info.get('request')
            }
    
    return None

def filter_logs(filename, filters):
    """
    Filter log lines based on the provided filters.

    Args:
        filename (str): Path to the log file.
        filters (dict): Dictionary of filters to apply (id, function).
    """
    with open(filename, 'r') as f:
        for line in f:
            line = line.strip()
            output = parse_log_line(line,filters=filters)
            
            if output:
                if output['type'] == 'request':
                    print(f"[{output['function']}] Request ID: {output['id']}")
                    print(f"  Parameters: {output['params']}")
                    print(f"  Request: {output['request']}\n")
                else:
                    print(f"[{output['function']}] Response ID: {output['id']}")
                    print(f"  Message: {output['message']}")
                    print(f"  Request: {output['request']}\n")
           

def main():
    """
    Main function to parse arguments and filter logs.
    """
    parser = argparse.ArgumentParser(description='Filter FUSE filesystem logs')
    parser.add_argument('logfile', help='Log file to process')
    parser.add_argument('--id', help='Filter by ID')
    parser.add_argument('--functions', nargs='+', help='Filter by function names (for request logs)')

    args = parser.parse_args()

    filters = {
        'id': args.id,
        'functions': args.functions
    }

    filter_logs(args.logfile, filters)

if __name__ == '__main__':
    main()